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
    if profile == "aggregate_operator_values_test":
        root = "AggregateOperatorValuesTest.dut."
        assert values(changes, root + "meta[-1]") == ["1xz0"]
        assert values(changes, root + "meta[0]") == ["0zx1"]
        assert values(changes, root + "flipped[3]") == ["0xx1"]
        assert values(changes, root + "flipped[2]") == ["1xx0"]
        assert values(changes, root + "literal_meta[7]") == ["1xx0"]
        assert values(changes, root + "literal_meta[6]") == ["0xx0"]
        assert values(changes, root + "masked[4]") == [f"{1 << 127:0128b}"]
        assert values(changes, root + "masked[3]") == [f"{2:0128b}"]
        assert values(changes, root + "inverted[2]") == [f"{(1 << 127) - 1:0128b}"]
        assert values(changes, root + "inverted[1]") == ["0" * 128]
        assert values(changes, root + "selected[5]") == ["1", "x"]
        assert values(changes, root + "selected[3]") == ["0", "x"]
        assert values(changes, root + "y[11]") == ["1", "x"]
        assert values(changes, root + "joined[3].n") == [f"{7:064b}"]
        assert values(changes, root + "joined[2].n") == [f"{12:064b}"]
    elif profile == "hardware_procedure_places_test":
        root = "HardwareProcedurePlacesTest.dut."
        assert values(changes, root + "packed_meta") == ["1xz0"]
        assert values(changes, root + "packed_result") == ["00000000", "10100011"]
        assert values(changes, root + "dynamic_bit") == ["00000000", "00001000", "00011000"]
        assert values(changes, root + "signal_twice") == [f"{n:0128b}" for n in (0, 1, 2)]
        assert values(changes, root + "selected") == [f"{n:0128b}" for n in (0, 1 << 127)]
        assert values(changes, root + "untouched") == ["0" * 128]
        assert values(changes, root + "local_result") == [f"{(1 << 127) + 2:0128b}"]
        assert values(changes, root + "local_tag") == [f"{ord('λ'):032b}"]
    elif profile == "fifo_test":
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
