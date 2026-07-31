#!/usr/bin/env python3
"""Modbus RTU replay over a PTY, for live-testing the mbshark2 GUI.

Usage (in two terminals):

  # 1. Create a virtual serial port pair:
  socat -d pty,raw,echo=0,link=/tmp/mb_a pty,raw,echo=0,link=/tmp/mb_b

  # 2. Launch mbshark2, select /tmp/mb_a in the settings panel, click Start.

  # 3. Replay traffic into the other end:
  python3 tools/replay_modbus.py /tmp/mb_b

Sends three scenarios that match the README:
  (a) normal request + response pair
  (b) request with no reply  -> timeout
  (c) late reply after the timeout -> ORPHAN

Stdlib only (no pymodbus / pyserial needed).
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

    # FC 0x10 (Write Multiple Registers): slave 2, start 0, count 2, values 0x0000 0x0001
    req = frame(2, 0x10, bytes([0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]))
    # Response: echoes start + count
    resp = frame(2, 0x10, bytes([0x00, 0x00, 0x00, 0x02]))

    # (a) Normal pair: request, 60 ms gap, response.
    print(">> normal pair")
    os.write(fd, req)
    time.sleep(0.06)
    os.write(fd, resp)
    time.sleep(0.5)

    # (b) Timeout: request, no reply. Let mbshark2's sweeper declare timeout.
    #     Use a fresh slave id (3) so it doesn't collide with the still-open
    #     transaction above.
    print(">> timeout (no reply)")
    req2 = frame(3, 0x10, bytes([0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]))
    os.write(fd, req2)
    # Wait well past the configured response_timeout (default 500 ms).
    time.sleep(1.2)

    # (c) Orphan: the late reply for slave 3 finally arrives.
    print(">> late reply -> ORPHAN")
    resp2 = frame(3, 0x10, bytes([0x00, 0x00, 0x00, 0x02]))
    os.write(fd, resp2)
    time.sleep(0.5)

    # (d) Exception response: request for slave 4, then exception (code 2).
    print(">> exception response")
    req3 = frame(4, 0x10, bytes([0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]))
    os.write(fd, req3)
    time.sleep(0.06)
    exc = frame(4, 0x90, bytes([0x02]))
    os.write(fd, exc)
    time.sleep(0.5)

    os.close(fd)
    print("done")


if __name__ == "__main__":
    main()
