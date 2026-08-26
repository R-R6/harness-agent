# supervise.ps1 —— 方案 A：纯脚本监督闭环
# 流程：claude -p 派活 → codex exec 审查（agent-sessions MCP）→ claude -p --resume 回灌 → 循环到验收
#
# 用法：
#   .\supervise.ps1 -Task "实现斐波那契函数" -WorkDir "C:\work\my-project"
#   .\supervise.ps1 -Task "实现斐波那契函数" -MaxRounds 5 -Model "gpt-5.6-luna"
#   .\supervise.ps1 -Task "实现斐波那契函数" -Mock            # 模拟模式（不真调 claude/codex）
#
# 参数：
#   -Task       任务描述（必填）
#   -WorkDir    工作目录（claude 干活的项目目录，默认当前目录）
#   -Level      任务分级 L0/L1/L2（默认 L1）：映射 MaxRounds 1/3/5 + 审查模型；-MaxRounds/-Model 显式传参可覆盖
#   -MaxRounds  最大循环轮数（默认 0 = 按 Level 推导）
#   -Model      codex 审查模型（默认空 = 按 Level 推导；见脚本顶部 $script:*_MODEL 常量）
#   -Mock       模拟模式：不调用真实 claude/codex，用本地伪造数据演练循环逻辑
#   -ResumeSessionId  可选：续接已有会话（跳过派活，直接审查-回灌）
#   -Tail       审查时读取的会话尾部条数（默认 60，省 token）
#   -ReviewPrompt  可选：自定义审查提示词模板（含 {file} 占位符）

param(
  [Parameter(Mandatory = $true)][string]$Task,
  [string]$WorkDir = (Get-Location).Path,
  [ValidateSet("L0", "L1", "L2")][string]$Level = "L1",
  [int]$MaxRounds = 0,          # 0 = 按 Level 推导（PS 5.1 无法区分"默认值"与"显式传参"，用 0 作哨兵）
  [string]$Model = "",          # 空 = 按 Level 推导（哨兵）
  [switch]$Mock,
  [string]$ResumeSessionId = "",
  [int]$Tail = 60,
  [string]$ReviewPrompt = ""
)

# 中文 Windows 默认 OEM 代码页(936)：管道 stdout 会编码成 GBK，宿主侧按 UTF-8 解码
# 即失败丢行。进程一启动就强制 stdout 走 UTF-8（无 BOM）。无控制台句柄时 setter
# 可能抛异常（GUI 宿主直接 spawn），降级为系统默认，行为与旧版一致。
try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch {}

$ErrorActionPreference = "Stop"
# $HOME 是 pwsh/PS 内建自动变量，跨平台：Windows 取 %USERPROFILE%，Unix 取 $env:HOME。
# 不能用 $env:USERPROFILE——Unix pwsh 无此变量，Join-Path $null 直接抛错。
# 路径分段用嵌套 Join-Path，两平台分隔符都正确。
$script:CLAUDE_PROJECTS = Join-Path (Join-Path $HOME ".claude") "projects"
$script:HOOK = "=" * 60

# 任务分级模型常量（Codex #13：默认全 luna；terra 需真实冒烟 `codex exec -m gpt-5.6-terra` 确认可用后启用）
$script:L0_MODEL = "gpt-5.6-luna"
$script:L1_MODEL = "gpt-5.6-luna"
$script:L2_MODEL = "gpt-5.6-luna"   # TODO: terra 冒烟确认后改 "gpt-5.6-terra"

# Level → 默认资源预算（可单测：parse-unit.test.ps1）
function Get-LevelDefaults {
  param([string]$Level, [int]$ExplicitMaxRounds, [string]$ExplicitModel)
  $l = $Level.ToUpper()
  $map = @{
    L0 = @{ rounds = 1; model = $script:L0_MODEL }
    L1 = @{ rounds = 3; model = $script:L1_MODEL }
    L2 = @{ rounds = 5; model = $script:L2_MODEL }
  }
  $d = $map[$l]
  if (-not $d) { throw "未知 Level: $Level（应为 L0/L1/L2）" }
  return @{
    maxRounds = if ($ExplicitMaxRounds -gt 0) { $ExplicitMaxRounds } else { $d.rounds }
    model = if ($ExplicitModel) { $ExplicitModel } else { $d.model }
  }
}

# codex -m 参数构造（可单测）：空模型返回空数组，杜绝 `codex ... -m ""` 传空参（R3）
function Get-CodexModelArgs {
  param([string]$Model)
  if ($Model) { return @("-m", $Model) } else { return @() }
}

# ---------------- 工具函数 ----------------

function Log($level, $msg) {
  $ts = Get-Date -Format "HH:mm:ss"
  Write-Output "[$ts][$level] $msg"
}

# 按 session_id 在 ~/.claude/projects 下找会话文件（Claude 的 JSON 输出只给 id，不给路径）
function Find-SessionFile($sessionId) {
  if (-not $sessionId) { return $null }
  Get-ChildItem -Path $script:CLAUDE_PROJECTS -Recurse -Filter "*.jsonl" -ErrorAction SilentlyContinue |
    Where-Object { $_.BaseName -eq $sessionId } |
    Select-Object -First 1 -ExpandProperty FullName
}

# 从 claude -p 输出提取 session_id / result
# 坑：claude 输出可能是 ①单对象JSON ②JSON数组[{...}] ③多行混合（含stderr行）
#     ConvertFrom-Json 逐行解析在这些情况下都会失败 → 用正则直接抓，不依赖JSON完整性
function Parse-ClaudeJson($raw) {
  $m = [regex]::Match($raw, '"session_id"\s*:\s*"([^"]+)"')
  if (-not $m.Success) { return $null }
  $rm = [regex]::Match($raw, '"result"\s*:\s*"((?:[^"\\]|\\.)*)"')
  return @{
    session_id = $m.Groups[1].Value
    result = if ($rm.Success) { $rm.Groups[1].Value } else { "" }
    type = "result"
  }
}

# 从 codex exec 输出中提取 [VERDICT] 行（PASS / REVIEW + 意见）
# 坑：codex 非 TTY 模式会回显提示词（含 "[VERDICT] PASS 或 [VERDICT] REVIEW"），
#     必须跳过回显行，并取最后一个匹配（真正的回复在输出末尾）
function Parse-CodexVerdict($raw) {
  $found = @()
  foreach ($line in ($raw -split "`n")) {
    $m = [regex]::Match($line, '\[VERDICT\]\s*(PASS|REVIEW)(.*)')
    if ($m.Success) {
      if ($line -match "最后一行必须输出") { continue }   # 跳过提示词回显
      $found += @{ verdict = $m.Groups[1].Value; reason = $m.Groups[2].Value.Trim() }
    }
  }
  if ($found.Count -eq 0) { return $null }
  return $found[$found.Count - 1]
}

# 审查提示词（默认模板）
function Get-ReviewPrompt($file) {
  if ($script:ReviewPrompt) { return $script:ReviewPrompt.Replace("{file}", $file) }
  return @"
你是监督者。用 agent-sessions MCP 的 get_transcript 读取会话文件：$file（tail $Tail 条）。
审查：1) 任务完成度 2) 方案合理性 3) 风险/遗漏。
最后一行必须输出 [VERDICT] PASS 或 [VERDICT] REVIEW + 一句具体返工指令（Claude 能直接执行）。
"@
}

# ---------------- 真实执行函数 ----------------

function Invoke-ClaudeWork {
  param([string]$TaskText)
  Log "INFO" "claude -p 派活: $TaskText"
  # --dangerously-skip-permissions：非交互模式工具调用默认要审批会卡死（实测 Write 卡"等待批准"）；
  # 本脚本只用于受控工作目录，配合 -WorkDir 使用
  $json = claude -p $TaskText --output-format json --max-turns 20 --dangerously-skip-permissions 2>&1
  $obj = Parse-ClaudeJson ($json -join "`n")
  if (-not $obj -or -not $obj.session_id) {
    throw "claude 派活失败，未能拿到 session_id。原始输出: $($json -join ' ' | Select-Object -First 300)"
  }
  $file = Find-SessionFile $obj.session_id
  if (-not $file) { throw "找不到会话文件: session_id=$($obj.session_id)" }
  Log "OK"   "claude 会话创建: $($obj.session_id) → $file"
  return @{ sessionId = $obj.session_id; file = $file; result = $obj.result }
}

function Invoke-CodexReview {
  param([string]$SessionFile)
  $prompt = Get-ReviewPrompt $SessionFile
  # exec 的 MCP 连接有 Windows 管道竞态（~50% 撞 Reconnecting），重试 5 次
  for ($i = 1; $i -le 5; $i++) {
    Log "INFO" "codex exec 审查（第 $i 次尝试）..."
    # 两个坑：① codex 在非 TTY 环境会读 stdin 附加输入而挂起 → $null | 置空管道立即 EOF
    # ② codex 把 "Reading additional input from stdin..." 写到 stderr，
    #    PowerShell 5.1 在 ErrorActionPreference=Stop 下会把 stderr 行当 NativeCommandError 抛异常
    #    → 调用期间临时降为 Continue
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
      # Get-CodexModelArgs：空模型时返回空数组（splat 展开不传参），杜绝 `-m ""`（R3）
      $modelArgs = Get-CodexModelArgs -Model $Model
      $out = $null | codex exec --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox @modelArgs $prompt 2>&1
    } finally {
      $ErrorActionPreference = $prev
    }
    $v = Parse-CodexVerdict ($out -join "`n")
    if ($v) {
      Log "OK" "codex 审查返回: $($v.verdict) $($v.reason)"
      return $v
    }
    Log "WARN" "未解析到 VERDICT（可能是竞态/限流），重试..."
    Start-Sleep -Seconds 8
  }
  throw "codex 审查连续 5 次失败，最后输出: $($out -join ' ' | Select-Object -First 300)"
}

function Invoke-ClaudeRework {
  param([string]$SessionId, [string]$Feedback)
  Log "INFO" "claude -p --resume 回灌: $Feedback"
  $json = claude -p $Feedback --resume $SessionId --output-format json --max-turns 20 --dangerously-skip-permissions 2>&1
  $obj = Parse-ClaudeJson ($json -join "`n")
  if (-not $obj) { throw "回灌失败（claude --resume 无输出）。原始输出: $($json -join ' ' | Select-Object -First 300)" }
  Log "OK" "回灌完成: $($obj.result)"
}

# ---------------- Mock 执行函数（模拟模式，不碰真实 claude/codex） ----------------

$script:MockFile = ""
$script:MockRound = 0

function New-MockSession {
  # 造一个伪 Claude 会话 JSONL（格式与真实一致）
  $mockDir = Join-Path $PSScriptRoot ".mock-data"
  if (-not (Test-Path $mockDir)) { New-Item -ItemType Directory -Path $mockDir -Force | Out-Null }
  $script:MockFile = Join-Path $mockDir ("mock-session-{0}.jsonl" -f (Get-Date -Format "HHmmss"))
  $lines = @(
    '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"' + $Task + '"}]},"timestamp":"2026-08-08T10:00:00.000Z"}',
    '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"好的，我完成了任务。"}]},"timestamp":"2026-08-08T10:00:30.000Z"}'
  )
  $lines | Set-Content -Path $script:MockFile -Encoding UTF8
  Log "OK" "mock 会话创建: $script:MockFile"
  return @{ sessionId = "mock-0001"; file = $script:MockFile }
}

function Invoke-MockReview {
  param([string]$SessionFile)
  $text = Get-Content $SessionFile -Raw -ErrorAction SilentlyContinue
  $script:MockRound++
  # 永不通过场景（sim-test 验证 MaxRounds/Level 用）：任务文本含"永不通过" → 永远 REVIEW
  if ($text -match "永不通过") {
    return @{ verdict = "REVIEW"; reason = "永不通过测试场景（模拟持续返工）"; round = $script:MockRound }
  }
  # 模拟监督逻辑：检查返工是否补上了"校验"和"测试"
  if ($text -notmatch "校验") {
    return @{ verdict = "REVIEW"; reason = "缺少输入校验，请补充边界校验。"; round = $script:MockRound }
  }
  if ($text -notmatch "测试") {
    return @{ verdict = "REVIEW"; reason = "缺少测试用例，请补充边界测试。"; round = $script:MockRound }
  }
  return @{ verdict = "PASS"; reason = "校验与测试均已补齐。"; round = $script:MockRound }
}

function Invoke-MockRework {
  param([string]$SessionFile, [string]$Feedback)
  $ts = (Get-Date).ToString("yyyy-MM-ddTHH:mm:ss.fffZ")
  # 一次只补一个缺陷：先补校验，再补测试 → 保证跑满三轮
  $text = Get-Content $SessionFile -Raw -ErrorAction SilentlyContinue
  $fix = if ($text -notmatch "校验") { "补充输入校验" } else { "补充边界测试" }
  $user = '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"监督意见: ' + $Feedback + '"}]},"timestamp":"' + $ts + '"}'
  $assistant = '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"已按监督意见修改：' + $fix + '。"}]},"timestamp":"' + $ts + '"}'
  Add-Content -Path $SessionFile -Value $user, $assistant -Encoding UTF8
  Log "OK" "mock 回灌完成（已追加监督意见与修改）"
}

# ---------------- 监督产物落盘（D1） ----------------
# 目录：$WorkDir\.supervise\（Mock 模式同样落盘，供 sim-test 断言）
# 编码策略（Codex #7）：md 用 UTF-8 with BOM（PS 5.1 中文兼容）；json 用 UTF-8 无 BOM（Node JSON.parse 直接可读）

function Get-ArtifactDir {
  $id = $env:SUPERVISE_TASK_ID
  if ([string]::IsNullOrWhiteSpace($id)) {
    throw "SUPERVISE_TASK_ID 未设置：拒绝清理整个 .supervise（会误删其他任务产物和 stop-markers）"
  }
  Join-Path (Get-Location) (Join-Path ".supervise" (Join-Path "tasks" $id))
}

function Reset-ArtifactDir {
  # 只清本任务子目录，禁止 Remove-Item .supervise -Recurse
  $dir = Get-ArtifactDir
  if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
}

function Write-ReviewArtifact {
  param([int]$Round, [string]$Verdict, [string]$Reason, [string]$Model, [string]$SessionId, [string]$File)
  $dir = Get-ArtifactDir
  if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
  $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
  $md = @"
# 第 $Round 轮审查意见

- 时间：$ts
- 审查模型：$Model
- 判定：$Verdict
- 会话：$SessionId
- 会话文件：$File

## 意见

$Reason
"@
  $mdPath = Join-Path $dir ("review-{0}.md" -f $Round)
  [System.IO.File]::WriteAllText($mdPath, $md, [System.Text.UTF8Encoding]::new($true))   # md：带 BOM
  Log "OK" "审查意见落盘: $mdPath"
  # 直接写 script 变量而非 return：Log 走 Write-Output 会混入返回值（实测坑：返回数组首元素是日志行，Join-Path 报 "Cannot find drive '[13"）
  $script:ArtifactDir = $dir
}

function Write-FinalReport {
  param([object]$Summary, [string]$Dir, [string]$Model)
  $jsonPath = Join-Path $Dir "final-report.json"
  [System.IO.File]::WriteAllText($jsonPath, ($Summary | ConvertTo-Json -Depth 6), [System.Text.UTF8Encoding]::new($false))  # json：无 BOM
  Log "OK" "监督报告落盘: $jsonPath"
}

# ---------------- 主循环 ----------------

function Run-Loop {
  # 阶段 0：清理旧监督产物（幂等，Codex #6）
  Reset-ArtifactDir
  $script:ArtifactDir = ""

  # 阶段 1：派活（或续接已有会话）
  if ($ResumeSessionId) {
    $file = Find-SessionFile $ResumeSessionId
    if (-not $file) { throw "找不到要续接的会话: $ResumeSessionId" }
    $sess = @{ sessionId = $ResumeSessionId; file = $file }
    Log "INFO" "续接已有会话: $ResumeSessionId"
  } elseif ($Mock) {
    $sess = New-MockSession
  } else {
    $sess = Invoke-ClaudeWork -TaskText $Task
  }

  # 阶段 2：审查-回灌循环
  $report = @()
  for ($round = 1; $round -le $MaxRounds; $round++) {
    Log "STEP" "$script:HOOK"
    Log "STEP" "第 $round/$MaxRounds 轮审查"
    Log "STEP" "$script:HOOK"

    if ($Mock) {
      $v = Invoke-MockReview -SessionFile $sess.file
    } else {
      $v = Invoke-CodexReview -SessionFile $sess.file
    }

    $report += [pscustomobject]@{
      round = $round
      verdict = $v.verdict
      reason = $v.reason
      sessionId = $sess.sessionId
      file = $sess.file
    }
    Log "INFO" "审查结果: $($v.verdict) — $($v.reason)"
    Write-ReviewArtifact -Round $round -Verdict $v.verdict -Reason $v.reason -Model $Model -SessionId $sess.sessionId -File $sess.file

    if ($v.verdict -eq "PASS") {
      Log "PASS" "验收通过（第 $round 轮）: $($v.reason)"
      break
    }

    if ($round -lt $MaxRounds) {
      if ($Mock) {
        Invoke-MockRework -SessionFile $sess.file -Feedback $v.reason
      } else {
        Invoke-ClaudeRework -SessionId $sess.sessionId -Feedback $v.reason
      }
    } else {
      Log "FAIL" "达到最大轮数 $MaxRounds，仍未通过验收。最后意见: $($v.reason)"
    }
  }

  # 阶段 3：汇总报告（JSON，供脚本/Hermes 解析；全部走 stdout，可被管道捕获）
  $final = $report | Select-Object -Last 1
  $summary = [pscustomobject]@{
    status = if ($final.verdict -eq "PASS") { "accepted" } else { "rejected" }
    task = $Task
    rounds = $report.Count
    sessionId = $sess.sessionId
    sessionFile = $sess.file
    verdicts = $report
  }
  Write-Output "`n=== 监督报告 ==="
  $summary | ConvertTo-Json -Depth 5
  if ($script:ArtifactDir) { Write-FinalReport -Summary $summary -Dir $script:ArtifactDir -Model $Model }
  return $summary
}

# ---------------- 入口 ----------------

# 点源守卫：. (dot-source) 调用时只加载函数定义（供解析单测点源），不执行主流程；
# 直接 & 调用或 -File 运行时 InvocationName 非 "."，正常执行主流程（行为不变）
if ($MyInvocation.InvocationName -ne ".") {
  # Level 推导：-MaxRounds/-Model 未显式传参（哨兵 0/空）时按 Level 映射
  $lv = Get-LevelDefaults -Level $Level -ExplicitMaxRounds $MaxRounds -ExplicitModel $Model
  $MaxRounds = $lv.maxRounds
  $Model = $lv.model
  Log "INFO" "Level=$Level → MaxRounds=$MaxRounds, Model=$Model"

  # 切到工作目录（claude 会在那里创建项目 slug）
  Push-Location $WorkDir
  try {
    $script:ReviewPrompt = $ReviewPrompt
    Run-Loop
  } finally {
    Pop-Location
  }
}
