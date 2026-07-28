#!/usr/bin/env python3
"""Small semantic checker for VCDs emitted by native corpus executables."""

from __future__ import annotations

import argparse
from pathlib import Path


def read_vcd(path: Path) -> tuple[dict[str, list[tuple[int, str]]], list[int]]:
    text = path.read_text()
    for required in ("$timescale 1fs $end", "$enddefinitions $end"):
        if required not in text:
            raise AssertionError(f"{path}: missing {required}")

    scopes: list[str] = []
    identifiers: dict[str, str] = {}
    changes: dict[str, list[tuple[int, str]]] = {}
    timestamps: list[int] = []
    now = 0
    for line in text.splitlines():
        if line.startswith("$scope "):
            scopes.append(line.split()[2])
        elif line.startswith("$upscope"):
            scopes.pop()
        elif line.startswith("$var "):
            fields = line.split()
            identifiers[fields[3]] = ".".join([*scopes, fields[4]])
        elif line.startswith("#"):
            now = int(line[1:])
            timestamps.append(now)
        elif line and line[0] in "01xzXZ":
            identifier = line[1:]
            if identifier in identifiers:
                changes.setdefault(identifiers[identifier], []).append((now, line[0].lower()))
        elif line and line[0] in "bBrRsS":
            value, identifier = line[1:].split(maxsplit=1)
            if identifier in identifiers:
                changes.setdefault(identifiers[identifier], []).append((now, value.lower()))

    if timestamps != sorted(timestamps):
        raise AssertionError(f"{path}: timestamps are not monotonic")
    return changes, timestamps


def values(changes: dict[str, list[tuple[int, str]]], path: str) -> list[str]:
    try:
        return [value for _, value in changes[path]]
    except KeyError as error:
        raise AssertionError(f"missing VCD signal {path}") from error


def check_profile(profile: str, changes: dict[str, list[tuple[int, str]]]) -> None:
    if profile == "fifo_test":
        assert values(changes, "FifoTest.d.count") == [
            "000",
            "001",
            "010",
            "011",
            "010",
            "001",
            "000",
        ]
        assert values(changes, "FifoTest.d.dout") == [
            "00000000",
            "00001011",
            "00010110",
            "00100001",
            "00000000",
        ]
        assert values(changes, "FifoTest.d.empty") == ["1", "0", "1"]
    elif profile == "regfile_test":
        assert values(changes, "T.d.regs[2]") == ["00000000", "01100011"]
        assert values(changes, "T.d.rdata") == ["00000000", "01100011"]
    elif profile == "spi_test":
        assert values(changes, "SpiTest.d.rx")[-1] == "10100101"
        assert values(changes, "SpiTest.d.busy") == ["0", "1", "0"]
        assert values(changes, "SpiTest.d.m.bits")[-1] == "1000"
    elif profile == "stream_test":
        assert values(changes, "StreamTest.dut.got") == ["00101010"]
        assert values(changes, "StreamTest.dut.wire.valid") == ["1"]
        assert values(changes, "StreamTest.dut.wire.data") == ["00101010"]
    elif profile == "protocol_view_traits_test":
        assert values(changes, "ProtocolViewTraitsTest.spi.controller_rx") == [
            "00000000",
            "00111100",
            "11100111",
        ]
        assert values(changes, "ProtocolViewTraitsTest.spi.peripheral_rx") == [
            "00000000",
            "10100101",
            "00010010",
        ]
        assert values(changes, "ProtocolViewTraitsTest.spi.selected") == ["0", "1", "0"]
        assert values(changes, "ProtocolViewTraitsTest.i2c.controller_sample") == [
            "1",
            "0",
            "1",
            "0",
        ]
        assert values(changes, "ProtocolViewTraitsTest.i2c.target_sample") == [
            "1",
            "0",
            "1",
            "0",
        ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("vcd", type=Path)
    parser.add_argument("--profile")
    args = parser.parse_args()
    changes, _ = read_vcd(args.vcd)
    if args.profile:
        check_profile(args.profile, changes)


if __name__ == "__main__":
    main()
