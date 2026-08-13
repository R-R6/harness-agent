// 时间格式化：统一按北京时间（Asia/Shanghai）显示，不依赖用户系统时区
const bjFormatter = new Intl.DateTimeFormat("zh-CN", {
  timeZone: "Asia/Shanghai",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
});

function partsOf(iso: string): Record<string, string> {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return {};
  const out: Record<string, string> = {};
  for (const p of bjFormatter.formatToParts(d)) {
    out[p.type] = p.value;
  }
  return out;
}

/** 北京时间短格式：MM-DD HH:mm（会话列表用） */
export function formatShort(iso: string): string {
  const p = partsOf(iso);
  if (!p.month) return iso;
  return `${p.month}-${p.day} ${p.hour}:${p.minute}`;
}

/** 北京时间完整格式：YYYY-MM-DD HH:mm:ss（详情/正文用） */
export function formatFull(iso: string): string {
  const p = partsOf(iso);
  if (!p.year) return iso;
  return `${p.year}-${p.month}-${p.day} ${p.hour}:${p.minute}:${p.second}`;
}
