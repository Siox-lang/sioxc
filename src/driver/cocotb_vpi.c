/* The VPI surface cocotb's GPI calls, implemented over the siox design ABI.
 *
 * cocotb does not talk to a simulator directly: it ships `libcocotbvpi_*.so`,
 * which registers itself through `vlog_startup_routines_bootstrap` and then
 * calls back into whatever `vpi_*` symbols the simulator exports. That is the
 * whole contract -- sixteen functions and a time loop -- and this file is the
 * siox side of it. The design underneath is reached through four entry points
 * the LLVM backend already emits (`sx_reset`, `sx_read`, `sx_set`,
 * `sx_settle`), so nothing here knows how a design is lowered.
 *
 * The generated companion file supplies the signal table (`sx_vpi_signals`,
 * `sx_vpi_signal_count`, `sx_vpi_top`); everything else is fixed.
 */
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "vpi_user.h"

/* --- the design, as the LLVM backend exports it ------------------------- */
extern void sx_reset(void);
extern void sx_settle(void);
extern uint64_t sx_read(uint32_t signal);
extern void sx_set(uint32_t signal, uint64_t value);
extern uint64_t sx_read_word(uint32_t signal, uint32_t word);
extern void sx_set_word(uint32_t signal, uint32_t word, uint64_t value);

/* --- the generated signal table ----------------------------------------- */
typedef struct {
    const char *path; /* full hierarchical siox path */
    unsigned width;   /* bits */
    unsigned char is_input;
} sx_vpi_signal;

extern const sx_vpi_signal sx_vpi_signals[];
extern const unsigned sx_vpi_signal_count;
extern const char *const sx_vpi_top;

extern void vlog_startup_routines_bootstrap(void);

/* --- objects ------------------------------------------------------------ */

typedef struct sx_obj {
    PLI_INT32 otype; /* vpiModule | vpiReg | vpiIterator | vpiConstant */
    int index;       /* signal index; -1 for a scope */
    int constant;    /* vpiConstant: the value it stands for */
    char name[128];
    char fullname[512];
    /* iterator state */
    struct sx_obj **items;
    int n, pos;
} sx_obj;

static sx_obj *scopes;
static unsigned scope_count;
static sx_obj *sigs;
static sx_obj *root;
/* Two constants per signal: the left and right bounds cocotb asks for with
 * `vpi_handle(vpiLeftRange/vpiRightRange)` and then reads as a value. */
static sx_obj *bounds;

static const char *leaf_of(const char *path) {
    const char *dot = strrchr(path, '.');
    return dot ? dot + 1 : path;
}

/* Register the scope named by the first `len` bytes of `path`, and every
 * ancestor of it, exactly once. siox hands us flat dotted paths; cocotb wants
 * a tree, and the tree is entirely recoverable from the names. */
static sx_obj *intern_scope(const char *path, size_t len) {
    for (unsigned i = 0; i < scope_count; i++)
        if (strlen(scopes[i].fullname) == len &&
            strncmp(scopes[i].fullname, path, len) == 0)
            return &scopes[i];
    sx_obj *s = &scopes[scope_count++];
    s->otype = vpiModule;
    s->index = -1;
    snprintf(s->fullname, sizeof s->fullname, "%.*s", (int)len, path);
    snprintf(s->name, sizeof s->name, "%s", leaf_of(s->fullname));
    return s;
}

/* Not static: building the handle tables is the one piece of setup a caller
 * other than `main` needs, and the value-layer test drives this file without
 * running the simulation loop. */
void sx_vpi_init(void) {
    /* An upper bound: every signal can contribute at most one scope per dot. */
    unsigned max_scopes = sx_vpi_signal_count * 4 + 8;
    scopes = calloc(max_scopes, sizeof *scopes);
    sigs = calloc(sx_vpi_signal_count ? sx_vpi_signal_count : 1, sizeof *sigs);
    bounds = calloc(sx_vpi_signal_count ? sx_vpi_signal_count * 2 : 2,
                    sizeof *bounds);
    root = intern_scope(sx_vpi_top, strlen(sx_vpi_top));
    for (unsigned i = 0; i < sx_vpi_signal_count; i++) {
        const char *path = sx_vpi_signals[i].path;
        for (const char *p = strchr(path, '.'); p; p = strchr(p + 1, '.'))
            intern_scope(path, (size_t)(p - path));
        sigs[i].otype = vpiReg;
        sigs[i].index = (int)i;
        snprintf(sigs[i].fullname, sizeof sigs[i].fullname, "%s", path);
        snprintf(sigs[i].name, sizeof sigs[i].name, "%s", leaf_of(path));
        /* siox indexes a vector with 0 as the least significant element, and
         * `vpiBinStrVal` below writes the most significant first. Reporting
         * `[width-1 : 0]` therefore makes cocotb's element indices land on the
         * same bits as siox's -- `dut.count.value[0]` and `count[0]` are both
         * the LSB -- while keeping `int()` of the whole vector correct. The
         * declared siox range (`unsigned[8]` is ascending 0..7) is source-level
         * metadata and deliberately not what is reported here. */
        unsigned w = sx_vpi_signals[i].width;
        bounds[2 * i].otype = vpiConstant;
        bounds[2 * i].index = -1;
        bounds[2 * i].constant = w ? (int)w - 1 : 0;
        bounds[2 * i + 1].otype = vpiConstant;
        bounds[2 * i + 1].index = -1;
        bounds[2 * i + 1].constant = 0;
    }
}

/* Is `path` directly inside `scope` (one level, not a grandchild)? */
static int directly_in(const char *path, const char *scope) {
    size_t n = strlen(scope);
    if (strncmp(path, scope, n) != 0 || path[n] != '.') return 0;
    return strchr(path + n + 1, '.') == NULL;
}

/* --- callbacks ---------------------------------------------------------- */

typedef struct sx_cb {
    struct sx_cb *next;
    int live;
    PLI_INT32 reason;
    PLI_INT32 (*cb_rtn)(p_cb_data);
    sx_obj *obj;
    uint64_t deadline;
    PLI_BYTE8 *user_data;
    uint64_t last;
    s_cb_data data;
    s_vpi_time time;
    s_vpi_value value;
} sx_cb;

/* Callback objects are allocated once and never reused, because the handle
 * `vpi_register_cb` returns outlives the callback itself: cocotb calls
 * `vpi_remove_cb` on a one-shot callback that has already fired. Recycling the
 * storage means that remove lands on whichever callback has since taken the
 * slot -- which presents as the simulator running out of pending timers one
 * step into the test, with no error anywhere. They are released at shutdown. */
static sx_cb *cb_list;
static uint64_t sim_time; /* femtoseconds */
static int finished;

static void run_cb(sx_cb *c) {
    c->data.reason = c->reason;
    c->data.cb_rtn = c->cb_rtn;
    c->data.obj = (vpiHandle)c->obj;
    c->time.type = vpiSimTime;
    c->time.high = (PLI_UINT32)(sim_time >> 32);
    c->time.low = (PLI_UINT32)(sim_time & 0xffffffffu);
    c->data.time = &c->time;
    c->value.format = vpiSuppressVal;
    c->data.value = &c->value;
    c->data.index = 0;
    c->data.user_data = c->user_data;
    if (c->cb_rtn) c->cb_rtn(&c->data);
}

static int call_cbs(PLI_INT32 reason) {
    int called = 0;
    for (sx_cb *c = cb_list; c; c = c->next) {
        if (!c->live || c->reason != reason) continue;
        c->live = 0; /* retire before running: it may re-register */
        run_cb(c);
        called = 1;
    }
    return called;
}

static uint64_t signal_word0(int index) {
    return sx_read((uint32_t)index);
}

static int call_value_cbs(void) {
    int called = 0;
    for (sx_cb *c = cb_list; c; c = c->next) {
        if (!c->live || c->reason != cbValueChange) continue;
        if (!c->obj || c->obj->index < 0) continue;
        uint64_t now = signal_word0(c->obj->index);
        if (now == c->last) continue;
        c->last = now;
        run_cb(c); /* value-change callbacks stay registered */
        called = 1;
    }
    return called;
}

static void settle_value_cbs(void) {
    while (call_value_cbs()) {
    }
}

static int next_timed(uint64_t *at) {
    int found = 0;
    for (sx_cb *c = cb_list; c; c = c->next) {
        if (!c->live || c->reason != cbAfterDelay) continue;
        if (!found || c->deadline < *at) *at = c->deadline;
        found = 1;
    }
    return found;
}

static void call_timed_cbs(void) {
    for (sx_cb *c = cb_list; c; c = c->next) {
        if (!c->live || c->reason != cbAfterDelay) continue;
        if (c->deadline > sim_time) continue;
        c->live = 0;
        run_cb(c);
    }
}

/* Unhandled requests are traced rather than guessed at: cocotb probes for
 * object kinds and properties from later standards than the ones named here,
 * and `SIOX_VPI_TRACE=1` is how you find out which. */
static void trace_unhandled(const char *call, PLI_INT32 code,
                            const char *object) {
    static int on = -1;
    if (on < 0) on = getenv("SIOX_VPI_TRACE") != NULL;
    if (on)
        fprintf(stderr, "[siox-vpi] %s: unhandled code %d on %s\n", call, code,
                object ? object : "(null)");
}

/* --- VPI ---------------------------------------------------------------- */

vpiHandle vpi_register_cb(p_cb_data d) {
    sx_cb *c = calloc(1, sizeof *c);
    if (!c) return NULL;
    c->live = 1;
    c->reason = d->reason;
    c->cb_rtn = d->cb_rtn;
    c->obj = (sx_obj *)d->obj;
    c->user_data = d->user_data;
    if (d->reason == cbAfterDelay && d->time) {
        uint64_t delta = ((uint64_t)d->time->high << 32) | d->time->low;
        c->deadline = sim_time + delta;
    }
    if (d->reason == cbValueChange && c->obj && c->obj->index >= 0)
        c->last = signal_word0(c->obj->index);
    c->next = cb_list;
    cb_list = c;
    return (vpiHandle)c;
}

PLI_INT32 vpi_remove_cb(vpiHandle h) {
    sx_cb *c = (sx_cb *)h;
    if (c) c->live = 0;
    return 1;
}

vpiHandle vpi_handle_by_name(PLI_BYTE8 *name, vpiHandle scope) {
    if (!name) return NULL;
    char full[512];
    if (scope) {
        sx_obj *s = (sx_obj *)scope;
        snprintf(full, sizeof full, "%s.%s", s->fullname, name);
    } else {
        snprintf(full, sizeof full, "%s", name);
    }
    for (unsigned i = 0; i < sx_vpi_signal_count; i++)
        if (strcmp(sigs[i].fullname, full) == 0) return (vpiHandle)&sigs[i];
    for (unsigned i = 0; i < scope_count; i++)
        if (strcmp(scopes[i].fullname, full) == 0) return (vpiHandle)&scopes[i];
    /* cocotb also asks for a bare leaf under the root. */
    for (unsigned i = 0; i < sx_vpi_signal_count; i++)
        if (directly_in(sigs[i].fullname, sx_vpi_top) &&
            strcmp(sigs[i].name, name) == 0)
            return (vpiHandle)&sigs[i];
    return NULL;
}

vpiHandle vpi_handle_by_index(vpiHandle object, PLI_INT32 indx) {
    (void)object;
    (void)indx;
    return NULL;
}

vpiHandle vpi_handle(PLI_INT32 type, vpiHandle ref) {
    if ((type == vpiLeftRange || type == vpiRightRange) && ref) {
        sx_obj *o = (sx_obj *)ref;
        if (o->index < 0) return NULL;
        return (vpiHandle)&bounds[2 * o->index + (type == vpiRightRange)];
    }
    if (type != vpiScope || !ref) {
        trace_unhandled("vpi_handle", type,
                        ref ? ((sx_obj *)ref)->fullname : NULL);
        return NULL;
    }
    sx_obj *o = (sx_obj *)ref;
    const char *dot = strrchr(o->fullname, '.');
    if (!dot) return NULL;
    for (unsigned i = 0; i < scope_count; i++)
        if (strlen(scopes[i].fullname) == (size_t)(dot - o->fullname) &&
            strncmp(scopes[i].fullname, o->fullname,
                    (size_t)(dot - o->fullname)) == 0)
            return (vpiHandle)&scopes[i];
    return NULL;
}

static sx_obj *new_iter(void) {
    sx_obj *it = calloc(1, sizeof *it);
    if (it) {
        it->otype = vpiIterator;
        it->index = -1;
    }
    return it;
}

vpiHandle vpi_iterate(PLI_INT32 type, vpiHandle ref) {
    sx_obj *it;
    if (ref == NULL) {
        /* `vpiInstance` is the SystemVerilog spelling of "an instantiated
         * thing"; cocotb tries it before falling back to `vpiModule`. Both mean
         * the design root here. */
        if (type != vpiModule && type != vpiInstance) {
            trace_unhandled("vpi_iterate", type, NULL);
            return NULL;
        }
        it = new_iter();
        if (!it) return NULL;
        it->items = calloc(1, sizeof(sx_obj *));
        it->items[0] = root;
        it->n = 1;
        return (vpiHandle)it;
    }
    sx_obj *scope = (sx_obj *)ref;
    if (scope->index >= 0) return NULL; /* signals have no children */
    if (type == vpiReg || type == vpiNet || type == vpiVariables) {
        it = new_iter();
        if (!it) return NULL;
        it->items = calloc(sx_vpi_signal_count ? sx_vpi_signal_count : 1,
                           sizeof(sx_obj *));
        for (unsigned i = 0; i < sx_vpi_signal_count; i++)
            if (directly_in(sigs[i].fullname, scope->fullname))
                it->items[it->n++] = &sigs[i];
        return (vpiHandle)it;
    }
    if (type == vpiModule || type == vpiInternalScope) {
        it = new_iter();
        if (!it) return NULL;
        it->items = calloc(scope_count ? scope_count : 1, sizeof(sx_obj *));
        for (unsigned i = 0; i < scope_count; i++)
            if (directly_in(scopes[i].fullname, scope->fullname))
                it->items[it->n++] = &scopes[i];
        return (vpiHandle)it;
    }
    trace_unhandled("vpi_iterate", type, scope->fullname);
    return NULL;
}

vpiHandle vpi_scan(vpiHandle iterator) {
    sx_obj *it = (sx_obj *)iterator;
    if (!it || it->otype != vpiIterator) return NULL;
    if (it->pos >= it->n) {
        free(it->items);
        free(it);
        return NULL;
    }
    return (vpiHandle)it->items[it->pos++];
}

PLI_INT32 vpi_get(PLI_INT32 property, vpiHandle object) {
    /* siox keeps femtosecond time, so the precision is exact, not scaled. */
    if (property == vpiTimePrecision || property == vpiTimeUnit) return -15;
    sx_obj *o = (sx_obj *)object;
    if (!o) return vpiUndefined;
    if (o->otype == vpiConstant)
        return property == vpiType ? vpiConstant
                                   : (property == vpiSize ? 32 : vpiUndefined);
    unsigned width = o->index >= 0 ? sx_vpi_signals[o->index].width : 0;
    switch (property) {
        case vpiType:
            return o->otype;
        case vpiSize:
            return (PLI_INT32)width;
        case vpiScalar:
            return o->index >= 0 && width == 1;
        case vpiVector:
            return o->index >= 0 && width > 1;
        case vpiDirection:
            if (o->index < 0) return vpiNoDirection;
            return sx_vpi_signals[o->index].is_input ? vpiInput : vpiOutput;
        default:
            trace_unhandled("vpi_get", property, o->fullname);
            return vpiUndefined;
    }
}

PLI_BYTE8 *vpi_get_str(PLI_INT32 property, vpiHandle object) {
    static char type_buf[32];
    sx_obj *o = (sx_obj *)object;
    if (!o) return NULL;
    switch (property) {
        case vpiName:
            return o->name;
        case vpiFullName:
            return o->fullname;
        case vpiType:
            snprintf(type_buf, sizeof type_buf, "%s",
                     o->otype == vpiModule ? "vpiModule" : "vpiReg");
            return type_buf;
        default:
            return NULL;
    }
}

/* A value wider than one machine word crosses the ABI a word at a time; the
 * buffer is sized for the widest signal the table declares. */
static char *value_buf;
static unsigned value_buf_len;

static void ensure_buf(unsigned bits) {
    unsigned want = bits + 2;
    if (want <= value_buf_len) return;
    free(value_buf);
    value_buf = malloc(want);
    value_buf_len = want;
}

static uint64_t word_of(int index, unsigned w) {
    return sx_read_word((uint32_t)index, w);
}

/* Decimal for a value past one machine word. `sx_read` returns a single word,
 * so anything that reaches for it here reports the low 64 bits of a 128-bit
 * signal as though that were the value -- the same silent truncation the
 * debugger's signal table had to grow widths to avoid.
 *
 * Long division by 10 rather than by 10^19: a 512-bit value takes ~155
 * iterations, which is free at the rate anyone asks for a formatted value, and
 * it avoids a second correctness argument about the last limb. `words` is
 * consumed destructively. */
static void wide_decimal(uint64_t *words, unsigned n, char *out, size_t cap) {
    char rev[700];
    size_t len = 0, i = 0;
    unsigned k;
    int any = 0;
    for (k = 0; k < n; k++)
        if (words[k]) any = 1;
    if (!any) {
        snprintf(out, cap, "0");
        return;
    }
    while (any && len < sizeof rev) {
        uint64_t rem = 0;
        int j;
        for (j = (int)n - 1; j >= 0; j--) {
            __uint128_t cur = ((__uint128_t)rem << 64) | words[j];
            words[j] = (uint64_t)(cur / 10);
            rem = (uint64_t)(cur % 10);
        }
        rev[len++] = (char)('0' + rem);
        any = 0;
        for (k = 0; k < n; k++)
            if (words[k]) any = 1;
    }
    while (len > 0 && i + 1 < cap) out[i++] = rev[--len];
    out[i] = 0;
}

void vpi_get_value(vpiHandle expr, p_vpi_value v) {
    sx_obj *o = (sx_obj *)expr;
    if (o && o->otype == vpiConstant) {
        v->format = vpiIntVal;
        v->value.integer = o->constant;
        return;
    }
    if (!o || o->index < 0) {
        v->format = vpiSuppressVal;
        return;
    }
    unsigned width = sx_vpi_signals[o->index].width;
    switch (v->format) {
        case vpiBinStrVal: {
            ensure_buf(width);
            for (unsigned i = 0; i < width; i++) {
                unsigned bit = width - 1 - i;
                uint64_t w = word_of(o->index, bit / 64);
                value_buf[i] = ((w >> (bit % 64)) & 1) ? '1' : '0';
            }
            value_buf[width] = 0;
            v->value.str = value_buf;
            break;
        }
        case vpiHexStrVal: {
            unsigned words = (width + 63) / 64, top, pos = 0;
            ensure_buf(width);
            top = words;
            while (top > 1 && word_of(o->index, top - 1) == 0) top--;
            pos += (unsigned)snprintf(value_buf + pos, value_buf_len - pos,
                                      "%llx",
                                      (unsigned long long)word_of(o->index,
                                                                  top - 1));
            for (unsigned k = top - 1; k > 0; k--)
                pos += (unsigned)snprintf(
                    value_buf + pos, value_buf_len - pos, "%016llx",
                    (unsigned long long)word_of(o->index, k - 1));
            v->value.str = value_buf;
            break;
        }
        case vpiDecStrVal: {
            unsigned words = (width + 63) / 64, k;
            ensure_buf(width);
            if (words == 1) {
                snprintf(value_buf, value_buf_len, "%llu",
                         (unsigned long long)sx_read((uint32_t)o->index));
            } else {
                uint64_t *scratch = malloc(words * sizeof *scratch);
                if (!scratch) {
                    v->format = vpiSuppressVal;
                    return;
                }
                for (k = 0; k < words; k++) scratch[k] = word_of(o->index, k);
                wide_decimal(scratch, words, value_buf, value_buf_len);
                free(scratch);
            }
            v->value.str = value_buf;
            break;
        }
        default:
            /* `vpiIntVal` is a 32-bit integer by definition (IEEE 1364), so
             * narrowing here is the format's own contract, not a loss this
             * layer is choosing. A caller that wants a wide value asks for a
             * string form. */
            v->format = vpiIntVal;
            v->value.integer = (PLI_INT32)sx_read((uint32_t)o->index);
            break;
    }
}

vpiHandle vpi_put_value(vpiHandle object, p_vpi_value v, p_vpi_time t,
                        PLI_INT32 flags) {
    (void)t;
    (void)flags;
    sx_obj *o = (sx_obj *)object;
    if (!o || o->index < 0) return NULL;
    unsigned width = sx_vpi_signals[o->index].width;
    if ((v->format == vpiHexStrVal || v->format == vpiDecStrVal) && width > 64) {
        /* `strtoull` saturates at one word, so a wide value written as text
         * would clamp to 2^64-1 rather than fail visibly. Only hex is
         * reconstructible without a bignum parser; decimal is refused rather
         * than silently wrong. */
        if (v->format == vpiDecStrVal) return NULL;
        size_t len = strlen(v->value.str);
        unsigned words = (width + 63) / 64, w;
        for (w = 0; w < words; w++) {
            uint64_t acc = 0;
            for (unsigned d = 0; d < 16; d++) {
                size_t digit = w * 16 + d; /* from the least significant end */
                if (digit >= len) break;
                char c = v->value.str[len - 1 - digit];
                uint64_t nib = (c >= '0' && c <= '9')   ? (uint64_t)(c - '0')
                               : (c >= 'a' && c <= 'f') ? (uint64_t)(c - 'a' + 10)
                               : (c >= 'A' && c <= 'F') ? (uint64_t)(c - 'A' + 10)
                                                        : 0;
                acc |= nib << (d * 4);
            }
            sx_set_word((uint32_t)o->index, w, acc);
        }
        return NULL;
    }
    if (v->format == vpiBinStrVal && width > 64) {
        /* Wide value: fill word by word, LSB-first, from the MSB-first text. */
        unsigned words = (width + 63) / 64;
        for (unsigned w = 0; w < words; w++) {
            uint64_t acc = 0;
            for (unsigned b = 0; b < 64; b++) {
                unsigned bit = w * 64 + b;
                if (bit >= width) break;
                size_t pos = width - 1 - bit;
                if (v->value.str[pos] == '1') acc |= (uint64_t)1 << b;
            }
            sx_set_word((uint32_t)o->index, w, acc);
        }
        return NULL;
    }
    uint64_t val = 0;
    switch (v->format) {
        case vpiBinStrVal:
            for (const char *p = v->value.str; *p; p++)
                val = (val << 1) | (*p == '1' ? 1u : 0u);
            break;
        case vpiHexStrVal:
            val = strtoull(v->value.str, NULL, 16);
            break;
        case vpiDecStrVal:
            val = strtoull(v->value.str, NULL, 10);
            break;
        default:
            val = (uint64_t)(uint32_t)v->value.integer;
            break;
    }
    sx_set((uint32_t)o->index, val);
    return NULL;
}

void vpi_get_time(vpiHandle object, p_vpi_time t) {
    (void)object;
    if (!t) return;
    t->type = vpiSimTime;
    t->high = (PLI_UINT32)(sim_time >> 32);
    t->low = (PLI_UINT32)(sim_time & 0xffffffffu);
    t->real = (double)sim_time;
}

PLI_INT32 vpi_free_object(vpiHandle object) {
    sx_obj *o = (sx_obj *)object;
    if (o && o->otype == vpiIterator) {
        free(o->items);
        free(o);
    }
    return 1;
}

PLI_INT32 vpi_control(PLI_INT32 operation, ...) {
    if (operation == vpiFinish || operation == vpiStop) finished = 1;
    return 1;
}

PLI_INT32 vpi_chk_error(p_vpi_error_info e) {
    (void)e;
    return 0;
}

PLI_INT32 vpi_get_vlog_info(p_vpi_vlog_info info) {
    static char product[] = "siox";
    static char version[] = SX_VPI_VERSION;
    static char *args[] = {product};
    if (!info) return 0;
    info->argc = 1;
    info->argv = args;
    info->product = product;
    info->version = version;
    return 1;
}

/* --- the time loop ------------------------------------------------------ */

int main(void) {
    /* cocotb needs to know which interpreter to embed. It is the one cocotb
     * was installed into, which the build already asked `cocotb-config` for --
     * so bake it in rather than make every invocation set it. An explicit
     * environment setting still wins. */
#ifdef SX_PYGPI_PYTHON_BIN
    if (!getenv("PYGPI_PYTHON_BIN"))
        setenv("PYGPI_PYTHON_BIN", SX_PYGPI_PYTHON_BIN, 1);
#endif
    sx_vpi_init();
    sx_reset();
    sx_settle();

    vlog_startup_routines_bootstrap();
    call_cbs(cbStartOfSimulation);
    settle_value_cbs();

    while (!finished) {
        sx_settle();
        settle_value_cbs();

        /* Writes cocotb scheduled during this step land here, then have to be
         * settled before anything reads them. */
        call_cbs(cbReadWriteSynch);
        sx_settle();
        settle_value_cbs();

        call_cbs(cbReadOnlySynch);
        if (finished) break;

        /* cocotb owns the clock, so the next thing to happen is whatever it
         * asked for. With nothing pending, the design cannot advance on its
         * own and the run is over. */
        uint64_t next = 0;
        if (!next_timed(&next)) break;
        if (next > sim_time) sim_time = next;

        call_cbs(cbNextSimTime);
        settle_value_cbs();
        call_timed_cbs();
        settle_value_cbs();
    }

    call_cbs(cbEndOfSimulation);
    for (sx_cb *c = cb_list; c;) {
        sx_cb *next = c->next;
        free(c);
        c = next;
    }
    free(value_buf);
    free(scopes);
    free(sigs);
    free(bounds);
    return 0;
}
