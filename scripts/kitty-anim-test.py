#!/usr/bin/env python3
"""Minimal Kitty graphics animation test.

Shows a 2-frame image (red / blue) and switches frames in different ways.
Watch whether the square blinks during each test.

    python3 scripts/kitty-anim-test.py
"""
import base64
import os
import sys
import time

ID = 4242
SIDE = 30  # 30x30 RGB -> 3600 base64 chars, fits in one chunk
SECONDS = 4
GAP = 0.4


def out(s):
    # One write() syscall per call, so we control how output is split.
    os.write(sys.stdout.fileno(), s.encode())


def solid(rgb):
    return base64.b64encode(bytes(rgb) * SIDE * SIDE).decode()


def label(n, text):
    out(f"\x1b[1;1H\x1b[2K\x1b[1mTest {n}:\x1b[0m {text}")
    out("\x1b[2;1H\x1b[2K(watch the square; does it blink red/blue?)")


def flip_loop(send):
    end = time.time() + SECONDS
    frame = 1
    while time.time() < end:
        frame = 2 if frame == 1 else 1
        send(frame)
        time.sleep(GAP)


def main():
    out("\x1b[?1049h\x1b[?25l\x1b[2J")
    try:
        # Root frame red, second frame blue (q=2: no replies on screen).
        out(f"\x1b_Ga=t,i={ID},f=24,s={SIDE},v={SIDE},q=2;{solid((220, 40, 40))}\x1b\\")
        out(f"\x1b_Ga=f,i={ID},f=24,s={SIDE},v={SIDE},z=400,q=2;{solid((40, 80, 220))}\x1b\\")
        out(f"\x1b[4;3H\x1b_Ga=p,i={ID},p=1,c=12,r=6,C=1,q=2\x1b\\")

        # 1. Terminal-driven playback: Ghostty flips frames on its own.
        label(1, "terminal-driven animation (a=a,s=3)")
        out(f"\x1b_Ga=a,i={ID},r=1,z=400,q=2\x1b\\")
        out(f"\x1b_Ga=a,i={ID},s=3,v=1,q=2\x1b\\")
        time.sleep(SECONDS)
        out(f"\x1b_Ga=a,i={ID},s=1,q=2\x1b\\")

        # 2. Client-driven, plain c=N.
        label(2, "client-driven a=a,c=N, nothing else")
        flip_loop(lambda f: out(f"\x1b_Ga=a,i={ID},c={f},q=2\x1b\\"))

        # 3. Like terminal-spinner today: sync begin / c=N / sync end
        #    as three separate writes.
        label(3, "c=N inside synchronized output, 3 separate writes")

        def split(f):
            out("\x1b[?2026h")
            out(f"\x1b_Ga=a,i={ID},c={f},q=2\x1b\\")
            out("\x1b[?2026l")

        flip_loop(split)

        # 4. Same, but one write.
        label(4, "c=N inside synchronized output, single write")
        flip_loop(lambda f: out(f"\x1b[?2026h\x1b_Ga=a,i={ID},c={f},q=2\x1b\\\x1b[?2026l"))

        # 5. c=N plus an invisible cell change (plain space <-> bold space)
        #    to force Ghostty to repaint text too.
        label(5, "c=N plus an invisible text change")
        flip_loop(
            lambda f: out(
                f"\x1b_Ga=a,i={ID},c={f},q=2\x1b\\"
                f"\x1b[3;1H{'' if f == 1 else chr(27) + '[1m'} \x1b[0m"
            )
        )

        label("-", "done. Note which tests blinked (1-5). Exiting in 3s.")
        out("\x1b[2;1H\x1b[2K")
        time.sleep(3)
    finally:
        out(f"\x1b_Ga=d,d=I,i={ID},q=2\x1b\\\x1b[?25h\x1b[?1049l")


if __name__ == "__main__":
    main()
