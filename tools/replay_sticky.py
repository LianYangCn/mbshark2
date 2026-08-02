#!/usr/bin/env python3
"""Sticky-packet (粘包) replay for live-testing mbshark2 against the
real-hardware failure mode: multiple RTU frames delivered in a single read.

Real serial stacks coalesce frames — the OS hands the application one buffer
containing bytes from several frames, all stamped with a single read time, so
the inter-frame gap is invisible. The old time-gap framer merged them into one
BadCrc parse failure. This script deterministically reproduces that by writing
multiple frames in a single `os.write()` call (and back-to-back with a sub-gap
delay), so you can verify the length+CRC split fix.

Usage (see README 脚本化支持):
  socat -d pty,raw,echo=0,link=/tmp/mb_a pty,raw,echo=0,link=/tmp/mb_b &
  MBSHARK_AUTOSTART_PORT=/tmp/mb_a MBSHARK_AUTOEXPORT_PATH=/tmp/capture.txt \
      cargo run --release
  python3 tools/replay_sticky.py /tmp/mb_b

Stdlib only.
"""
import os
import sys
import time


def crc16(buf: bytes) -> int:
    crc = 0xFFFF
    for b in buf:
        crc ^= b
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xA001
            else:
                crc >>= 1
    return crc


def frame(slave: int, fc: int, data: bytes) -> bytes:
    body = bytes([slave, fc]) + data
    c = crc16(body)
    return body + bytes([c & 0xFF, (c >> 8) & 0xFF])


def main() -> None:
    path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/mb_b"
    fd = os.open(path, os.O_RDWR)

    # FC 0x10 (Write Multiple Registers) request (13 B) + response (8 B).
    req = frame(2, 0x10, bytes([0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]))
    resp = frame(2, 0x10, bytes([0x00, 0x00, 0x00, 0x02]))
    # Two read-holding-registers requests (8 B each), distinct slaves.
    r1 = frame(5, 0x03, bytes([0x00, 0x00, 0x00, 0x0A]))
    r2 = frame(6, 0x03, bytes([0x00, 0x00, 0x00, 0x0A]))

    # (a) Coalesced request + response in ONE write — the canonical real-hw
    #     failure. Old framer: one 21-byte candidate -> BadCrc. New: two frames.
    blob = req + resp
    print(f">> (a) coalesced req+resp: {len(blob)} bytes in one write")
    os.write(fd, blob)
    time.sleep(0.6)

    # (b) Two requests coalesced in one write (no response -> both time out).
    blob2 = r1 + r2
    print(f">> (b) coalesced two requests: {len(blob2)} bytes in one write")
    os.write(fd, blob2)
    time.sleep(0.6)

    # (c) Back-to-back writes 1 ms apart (sub 3.5-char gap at any baud) — the
    #     old gap timer would reset on each read and merge them.
    print(">> (c) two requests 1 ms apart (sub-gap)")
    os.write(fd, r1)
    time.sleep(0.001)
    os.write(fd, r2)
    time.sleep(0.3)

    os.close(fd)
    print("done")


if __name__ == "__main__":
    main()
