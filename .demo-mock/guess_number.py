"""猜数字游戏：程序随机想一个 1~100 的整数，玩家反复猜，直到猜对。"""

import random
import sys

LOW, HIGH = 1, 100


def get_guess():
    """读取玩家输入并校验：非空、必须是整数、必须在 1~100 范围内。

    EOF（Ctrl+D / 输入流结束）和中断（Ctrl+C）会在此抛出，交由 main() 统一处理。
    """
    while True:
        raw = input(f"猜一个 {LOW}~{HIGH} 之间的整数: ").strip()
        if not raw:
            print("输入不能为空，请重新输入。")
            continue
        try:
            guess = int(raw)
        except ValueError:
            print(f'无效输入："{raw}" 不是整数，请重新输入。')
            continue
        if guess < LOW or guess > HIGH:
            print(f"数字超出范围，请输入 {LOW}~{HIGH} 之间的整数。")
            continue
        return guess


def main(answer=None):
    """运行一轮游戏，返回进程退出码。

    answer 可注入以便测试；默认随机生成 1~100 的整数。
    返回 0 表示正常结束（猜中或收到 EOF），130 表示被中断。
    """
    target = answer if answer is not None else random.randint(LOW, HIGH)
    attempts = 0
    print(f"我已想好一个 {LOW}~{HIGH} 之间的数字，开始猜吧！")

    try:
        while True:
            guess = get_guess()
            attempts += 1
            if guess < target:
                print("太小了，再大一点。")
            elif guess > target:
                print("太大了，再小一点。")
            else:
                print(f"恭喜！你猜对了，答案是 {target}，共猜了 {attempts} 次。")
                return 0
    except EOFError:
        print("\n已收到 EOF，游戏结束。")
        return 0
    except KeyboardInterrupt:
        print("\n已中断，游戏结束。")
        return 130


if __name__ == "__main__":
    sys.exit(main())
