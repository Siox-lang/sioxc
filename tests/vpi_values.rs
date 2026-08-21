//! The VPI value layer, tested without cocotb.
//!
//! `tests/cocotb_simulator.rs` is the real integration test, but it needs
//! cocotb installed and so does not run in CI. That leaves the part most likely
//! to be quietly wrong — converting values between siox's word ABI and VPI's
//! formats — covered only on a developer's machine.
//!
//! This links `cocotb_vpi.c` against a stub design and calls `vpi_get_value` /
//! `vpi_put_value` directly, so every format is exercised anywhere the tests
//! run. `main` is renamed away at compile time; the harness supplies its own.

use std::path::PathBuf;
use std::process::Command;

const HARNESS: &str = r#"
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include "vpi_user.h"

/* --- a stub design: three signals, one per interesting width -------------- */
typedef struct { const char *path; unsigned width; unsigned char is_input; } sx_vpi_signal;
const sx_vpi_signal sx_vpi_signals[] = {
    { "T.small", 8, 1 },
    { "T.wide", 128, 1 },
    { "T.edge", 65, 1 },
};
const unsigned sx_vpi_signal_count = 3;
const char *const sx_vpi_top = "T";

static uint64_t state[3][2];
static unsigned words_of(uint32_t s) { return (sx_vpi_signals[s].width + 63) / 64; }

void sx_reset(void) { memset(state, 0, sizeof state); }
void sx_settle(void) {}
uint64_t sx_read(uint32_t s) { return state[s][0]; }
void sx_set(uint32_t s, uint64_t v) { state[s][0] = v; state[s][1] = 0; }
uint64_t sx_read_word(uint32_t s, uint32_t w) { return w < words_of(s) ? state[s][w] : 0; }
void sx_set_word(uint32_t s, uint32_t w, uint64_t v) { if (w < words_of(s)) state[s][w] = v; }
void vlog_startup_routines_bootstrap(void) {}
extern void sx_vpi_init(void);

/* --- checks -------------------------------------------------------------- */
static int failures;
static void expect(const char *what, const char *got, const char *want) {
    if (strcmp(got, want) != 0) {
        printf("FAIL %s: got `%s` want `%s`\n", what, got, want);
        failures++;
    } else {
        printf("ok   %s = %s\n", what, got);
    }
}

static vpiHandle by_name(const char *n) { return vpi_handle_by_name((PLI_BYTE8 *)n, NULL); }

static const char *get_str(vpiHandle h, PLI_INT32 fmt) {
    static s_vpi_value v;
    v.format = fmt;
    vpi_get_value(h, &v);
    return v.value.str;
}

static void put_str(vpiHandle h, PLI_INT32 fmt, const char *text) {
    s_vpi_value v;
    v.format = fmt;
    v.value.str = (PLI_BYTE8 *)text;
    vpi_put_value(h, &v, NULL, vpiNoDelay);
}

int main(void) {
    sx_vpi_init();
    sx_reset();
    vpiHandle small = by_name("T.small"), wide = by_name("T.wide"), edge = by_name("T.edge");
    if (!small || !wide || !edge) { printf("FAIL: handles\n"); return 1; }

    /* A value inside one word behaves the obvious way. */
    put_str(small, vpiDecStrVal, "200");
    expect("small dec", get_str(small, vpiDecStrVal), "200");
    expect("small hex", get_str(small, vpiHexStrVal), "c8");
    expect("small bin", get_str(small, vpiBinStrVal), "11001000");

    /* A value past one word must not report its low word. This is the case
       that was wrong: hex and dec both read `sx_read`, so 2^64 + 7 printed
       as 7. */
    sx_set_word(1, 0, 7);
    sx_set_word(1, 1, 1);
    expect("wide hex", get_str(wide, vpiHexStrVal), "10000000000000007");
    expect("wide dec", get_str(wide, vpiDecStrVal), "18446744073709551623");

    /* Round trip through each string form. */
    put_str(wide, vpiHexStrVal, "deadbeefcafebabe0123456789abcdef");
    expect("wide hex round trip", get_str(wide, vpiHexStrVal),
           "deadbeefcafebabe0123456789abcdef");
    {
        char bits[129];
        for (int i = 0; i < 128; i++) bits[i] = '0';
        bits[128] = 0;
        bits[0] = '1';   /* bit 127 */
        bits[127] = '1'; /* bit 0   */
        put_str(wide, vpiBinStrVal, bits);
        expect("wide bin round trip", get_str(wide, vpiBinStrVal), bits);
        expect("wide bin as hex", get_str(wide, vpiHexStrVal),
               "80000000000000000000000000000001");
    }

    /* A width that is not a multiple of 64 still spans both words. */
    sx_set_word(2, 0, 0);
    sx_set_word(2, 1, 1);
    expect("edge hex", get_str(edge, vpiHexStrVal), "10000000000000000");
    expect("edge dec", get_str(edge, vpiDecStrVal), "18446744073709551616");

    /* Zero is its own case in the decimal conversion. */
    sx_set_word(1, 0, 0);
    sx_set_word(1, 1, 0);
    expect("wide zero dec", get_str(wide, vpiDecStrVal), "0");
    expect("wide zero hex", get_str(wide, vpiHexStrVal), "0");

    printf(failures ? "FAILURES\n" : "ALL OK\n");
    return failures ? 1 : 0;
}
"#;

#[test]
fn the_vpi_value_layer_converts_every_format() {
    let dir = std::env::temp_dir().join(format!("siox_vpival_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/driver");
    let harness = dir.join("harness.c");
    std::fs::write(&harness, HARNESS).unwrap();
    let bin: PathBuf = dir.join("vpival");

    // The VPI file owns `main` for the real simulator and the harness supplies
    // its own, so the rename has to apply to that translation unit alone --
    // defining it for both renames the one we want to keep.
    let obj = dir.join("cocotb_vpi.o");
    let compiled = Command::new("clang")
        .arg("-O1")
        .arg("-c")
        .arg("-o")
        .arg(&obj)
        .arg(format!("{src}/cocotb_vpi.c"))
        .arg(format!("-I{src}"))
        .arg("-Dmain=sx_vpi_unused_main")
        .arg("-DSX_VPI_VERSION=\"test\"")
        .output()
        .unwrap();
    assert!(
        obj.exists(),
        "the VPI layer did not compile:\n{}{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let built = Command::new("clang")
        .arg("-O1")
        .arg("-o")
        .arg(&bin)
        .arg(&harness)
        .arg(&obj)
        .arg(format!("-I{src}"))
        .output()
        .unwrap();
    assert!(
        bin.exists(),
        "the VPI harness did not build:\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );

    let ran = Command::new(&bin).output().unwrap();
    let text = String::from_utf8_lossy(&ran.stdout).to_string();
    assert!(
        text.contains("ALL OK"),
        "the value layer disagreed with the design's words:\n{text}"
    );
}
