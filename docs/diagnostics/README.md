# Diagnostic explanations

Run `sioxc --explain E-P003` to print a page without compiling a file or loading
std. The same pages are embedded in the compiler and available through
`siox::diag::codes::explanation(code)`; no network or installed source tree is
needed at runtime. Unknown codes return no page and the CLI exits unsuccessfully.

Every declaration in `diag::codes` generates its explanation lookup and the
`ALL` catalogue. Adding a code without its Markdown page fails the build.
Pages include an example and a fix, checked by the library tests. Examples
reuse the diagnostic UI corpus; compile testbench examples with `--test`.

Warnings can be promoted to errors without changing their `W-` code. A reserved
retired code has a page explaining its history; removed code constants are not
new active diagnostics. E-P028 no longer rejects multiple test processes.

| Code | Meaning |
| --- | --- |
| [E-P001](E-P001.md) | Unknown name |
| [E-P002](E-P002.md) | Duplicate declaration |
| [E-P003](E-P003.md) | Incompatible types or shapes |
| [E-P004](E-P004.md) | Writing an input |
| [E-P006](E-P006.md) | Attribute on an invalid target |
| [E-P007](E-P007.md) | Wrong attribute value type |
| [E-P008](E-P008.md) | No matching method |
| [E-P009](E-P009.md) | Invalid pattern |
| [E-P010](E-P010.md) | Analogue syntax is unavailable |
| [E-P011](E-P011.md) | Unresolved import |
| [E-P012](E-P012.md) | Missing binding type |
| [E-P013](E-P013.md) | An entity is not a constant |
| [E-P014](E-P014.md) | Conflicting drivers |
| [E-P015](E-P015.md) | Hardware recursion has no finite expansion |
| [E-P016](E-P016.md) | Access to a private declaration |
| [E-P017](E-P017.md) | Unsupported expression or storage form |
| [E-P018](E-P018.md) | Invalid assignment target |
| [E-P019](E-P019.md) | Statement without an effect |
| [E-P020](E-P020.md) | Entity instance in a nonstructural location |
| [E-P021](E-P021.md) | Nonconstant signal initializer |
| [E-P022](E-P022.md) | Reference to an omitted instance |
| [E-P023](E-P023.md) | Compile-time file input failed |
| [E-P024](E-P024.md) | Private member access |
| [E-P025](E-P025.md) | Private type in a public interface |
| [E-P026](E-P026.md) | Inherent implementation ownership |
| [E-P027](E-P027.md) | Process in an invalid container |
| [E-P028](E-P028.md) | Retired single-process scheduler restriction |
| [E-P029](E-P029.md) | Attribute object not found |
| [E-P030](E-P030.md) | Attribute bound more than once |
| [E-P031](E-P031.md) | Attribute has no value |
| [E-P032](E-P032.md) | Metadata written as a directive |
| [E-P033](E-P033.md) | Attempt to lower a forbidden lint |
| [E-P034](E-P034.md) | Malformed lint directive |
| [E-P035](E-P035.md) | Macro expansion failed |
| [E-P036](E-P036.md) | Reserved compiler hook misuse |
| [E-P037](E-P037.md) | Invalid type attribute |
| [E-P038](E-P038.md) | Unsupported recursive call frame |
| [E-P039](E-P039.md) | Match expression has no value for some cases |
| [W-P002](W-P002.md) | Possible combinational latch |
| [W-P003](W-P003.md) | Unused internal signal |
| [W-P004](W-P004.md) | Unused generic parameter |
| [W-P005](W-P005.md) | Unused import |
| [W-P006](W-P006.md) | Unreachable match arm |
| [W-P007](W-P007.md) | Incomplete match statement |
| [W-P008](W-P008.md) | Enum compared to a bare integer |
| [W-P009](W-P009.md) | Edge-detected reset |
| [W-P010](W-P010.md) | Combinational feedback loop |
| [W-P011](W-P011.md) | Undriven output or signal |
| [W-P012](W-P012.md) | Unconnected child input |
| [W-P014](W-P014.md) | Earlier assignment is overridden |
| [W-P015](W-P015.md) | Metadata has no implemented effect |
| [W-P016](W-P016.md) | Struct literal omits fields |
| [W-P017](W-P017.md) | Unknown lint name |
| [W-P018](W-P018.md) | Match ranges share an endpoint |
| [W-P019](W-P019.md) | Runtime hardware-loop bound |
