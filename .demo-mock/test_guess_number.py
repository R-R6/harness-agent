"""猜数字游戏的验证脚本。

覆盖四类行为：
  1. 非法输入（空 / 非整数 / 越界）→ 反复提示；
  2. 提示分支（太小 / 太大）；
  3. 猜中 → 退出码 0（含真实子进程退出码）；
  4. EOF / 中断处理 → 优雅退出、无 traceback。

运行：python test_guess_number.py
"""

import os
import subprocess
import sys
import unittest
from unittest import mock

import guess_number

HERE = os.path.dirname(os.path.abspath(__file__))


def run_main(answer, inputs):
    """用给定输入序列驱动 main()，返回 (退出码, 所有 print 输出行)。"""
    outputs = []
    it = iter(inputs)

    def fake_input(prompt=""):
        return next(it)

    def fake_print(*args, **kwargs):
        outputs.append(" ".join(str(a) for a in args))

    with mock.patch("builtins.input", side_effect=fake_input), \
            mock.patch("builtins.print", side_effect=fake_print):
        code = guess_number.main(answer=answer)
    return code, outputs


class GuessNumberTest(unittest.TestCase):
    def test_illegal_inputs_then_win(self):
        code, out = run_main(42, ["", "abc", "0", "200", "42"])
        self.assertEqual(code, 0)
        self.assertIn("输入不能为空，请重新输入。", out)
        self.assertIn('无效输入："abc" 不是整数，请重新输入。', out)
        self.assertEqual(out.count("数字超出范围，请输入 1~100 之间的整数。"), 2)
        self.assertIn("恭喜！你猜对了，答案是 42，共猜了 1 次。", out)

    def test_too_small_hint(self):
        code, out = run_main(50, ["10", "50"])
        self.assertEqual(code, 0)
        self.assertIn("太小了，再大一点。", out)
        self.assertIn("恭喜！你猜对了，答案是 50，共猜了 2 次。", out)

    def test_too_big_hint(self):
        code, out = run_main(50, ["90", "50"])
        self.assertEqual(code, 0)
        self.assertIn("太大了，再小一点。", out)
        self.assertIn("恭喜！你猜对了，答案是 50，共猜了 2 次。", out)

    def test_win_returns_zero(self):
        code, out = run_main(42, ["42"])
        self.assertEqual(code, 0)
        self.assertIn("恭喜！你猜对了，答案是 42，共猜了 1 次。", out)

    def test_eof_handling(self):
        outputs = []
        calls = {"n": 0}

        def fake_input(prompt=""):
            calls["n"] += 1
            if calls["n"] == 1:
                return "10"
            raise EOFError

        with mock.patch("builtins.input", side_effect=fake_input), \
                mock.patch("builtins.print",
                           side_effect=lambda *a, **k: outputs.append(" ".join(map(str, a)))):
            code = guess_number.main(answer=50)
        self.assertEqual(code, 0)
        self.assertTrue(any("已收到 EOF" in line for line in outputs))

    def test_interrupt_handling(self):
        outputs = []

        def fake_input(prompt=""):
            raise KeyboardInterrupt

        with mock.patch("builtins.input", side_effect=fake_input), \
                mock.patch("builtins.print",
                           side_effect=lambda *a, **k: outputs.append(" ".join(map(str, a)))):
            code = guess_number.main(answer=50)
        self.assertEqual(code, 130)
        self.assertTrue(any("已中断" in line for line in outputs))

    def test_subprocess_zero_exit_on_win(self):
        code = "import sys, guess_number; sys.exit(guess_number.main(answer=42))"
        result = subprocess.run(
            [sys.executable, "-c", code],
            input="10\n80\n42\n",
            capture_output=True,
            text=True,
            cwd=HERE,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("恭喜！你猜对了，答案是 42，共猜了 3 次。", result.stdout)

    def test_subprocess_eof_exit_cleanly(self):
        code = "import sys, guess_number; sys.exit(guess_number.main(answer=42))"
        result = subprocess.run(
            [sys.executable, "-c", code],
            input="10\n",  # 输入流提前结束
            capture_output=True,
            text=True,
            cwd=HERE,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertIn("已收到 EOF", result.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
