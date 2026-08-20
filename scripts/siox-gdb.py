"""Read siox signals by name inside gdb.

Load it against a binary built with `sioxc --test -g`:

    (gdb) source scripts/siox-gdb.py
    (gdb) siox print FifoTest.f.used
    FifoTest.f.used = 3
    (gdb) siox list f.mem
    FifoTest.f.mem[0] = 11
    FifoTest.f.mem[1] = 22

A hardware signal is not a variable — it lives behind the `sx_read` accessor,
indexed by `SignalId` — so DWARF has nothing natural to describe and printing
one by its siox path needs this mapping. A debug build carries
`sx_signal_names`, the hierarchical path of every signal in `SignalId` order,
and the value comes from calling `sx_read` in the running program.

Only a debug build carries the table; against an ordinary build these commands
say so rather than printing something misleading.
"""

import gdb


def _names():
    """Every signal path, indexed by SignalId, or None without the table."""
    try:
        count = int(gdb.parse_and_eval("sx_signal_count"))
        table = gdb.parse_and_eval("sx_signal_names")
    except gdb.error:
        return None
    out = []
    for index in range(count):
        value = table[index]
        # `string()` reads the inferior's memory for the char*.
        out.append(value.string() if value else "")
    return out


def _read(index):
    """The signal's current value, from the program itself."""
    return int(gdb.parse_and_eval(f"sx_read({index})"))


def _no_table():
    gdb.write(
        "no siox signal table in this binary; rebuild with `sioxc --test -g`\n",
        gdb.STDERR,
    )


class SioxPrint(gdb.Command):
    """siox print <path> — one signal, by its full or trailing path."""

    def __init__(self):
        super().__init__("siox print", gdb.COMMAND_DATA)

    def invoke(self, argument, from_tty):
        argument = argument.strip()
        if not argument:
            gdb.write("usage: siox print <signal path>\n", gdb.STDERR)
            return
        names = _names()
        if names is None:
            _no_table()
            return
        # An exact path wins; otherwise a unique suffix match, so `f.used` finds
        # `FifoTest.f.used` without typing the root.
        matches = [i for i, name in enumerate(names) if name == argument]
        if not matches:
            matches = [
                i
                for i, name in enumerate(names)
                if name == argument or name.endswith("." + argument)
            ]
        if not matches:
            gdb.write(f"no signal matches `{argument}`\n", gdb.STDERR)
            return
        if len(matches) > 1:
            gdb.write(f"`{argument}` is ambiguous:\n", gdb.STDERR)
            for index in matches:
                gdb.write(f"    {names[index]}\n", gdb.STDERR)
            return
        index = matches[0]
        gdb.write(f"{names[index]} = {_read(index)}\n")


class SioxList(gdb.Command):
    """siox list [prefix] — every signal, or those under a path."""

    def __init__(self):
        super().__init__("siox list", gdb.COMMAND_DATA)

    def invoke(self, argument, from_tty):
        argument = argument.strip()
        names = _names()
        if names is None:
            _no_table()
            return
        shown = 0
        for index, name in enumerate(names):
            if argument and argument not in name:
                continue
            gdb.write(f"{name} = {_read(index)}\n")
            shown += 1
        if not shown:
            gdb.write(f"no signal matches `{argument}`\n", gdb.STDERR)


class Siox(gdb.Command):
    """The `siox` command prefix."""

    def __init__(self):
        super().__init__("siox", gdb.COMMAND_DATA, prefix=True)

    def invoke(self, argument, from_tty):
        gdb.write("usage: siox print <path> | siox list [prefix]\n", gdb.STDERR)


Siox()
SioxPrint()
SioxList()
