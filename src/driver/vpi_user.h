/* The VPI (IEEE 1364-2005) declarations cocotb's GPI actually calls.
 *
 * Written rather than vendored: a simulator only has to satisfy the subset its
 * consumer uses, and cocotb's `libcocotbvpi_*.so` leaves exactly sixteen
 * `vpi_*` symbols undefined. Names, signatures and constant values are fixed
 * by the standard; nothing here is a siox invention. */
#ifndef SX_VPI_USER_H
#define SX_VPI_USER_H

#ifdef __cplusplus
extern "C" {
#endif

typedef int PLI_INT32;
typedef unsigned int PLI_UINT32;
typedef char PLI_BYTE8;
typedef short PLI_INT16;
typedef unsigned short PLI_UINT16;
typedef unsigned char PLI_UBYTE8;
typedef void PLI_VOID;

typedef PLI_UINT32 *vpiHandle;

/* object types */
#define vpiConstant 7
#define vpiIntegerVar 25
#define vpiIterator 27
#define vpiModule 32
#define vpiNet 36
#define vpiParameter 41
#define vpiPort 44
#define vpiReg 48
#define vpiScope 84
#define vpiInternalScope 92
#define vpiVariables 100
#define vpiNetArray 114
#define vpiRegArray 116
#define vpiLeftRange 79
#define vpiRightRange 83
/* IEEE 1800 (SystemVerilog): cocotb probes for it before `vpiModule`. */
#define vpiInstance 745

/* properties */
#define vpiUndefined -1
#define vpiType 1
#define vpiName 2
#define vpiFullName 3
#define vpiSize 4
#define vpiTimeUnit 11
#define vpiTimePrecision 12
#define vpiScalar 17
#define vpiVector 18
#define vpiDirection 20
#define vpiInput 1
#define vpiOutput 2
#define vpiInout 3
#define vpiNoDirection 5

/* control */
#define vpiStop 66
#define vpiFinish 67

/* time */
#define vpiScaledRealTime 1
#define vpiSimTime 2
#define vpiSuppressTime 3

/* value formats */
#define vpiBinStrVal 1
#define vpiOctStrVal 2
#define vpiDecStrVal 3
#define vpiHexStrVal 4
#define vpiScalarVal 5
#define vpiIntVal 6
#define vpiRealVal 7
#define vpiStringVal 8
#define vpiVectorVal 9
#define vpiStrengthVal 10
#define vpiTimeVal 11
#define vpiObjTypeVal 12
#define vpiSuppressVal 13

/* delay modes */
#define vpiNoDelay 1
#define vpiInertialDelay 2

/* callback reasons */
#define cbValueChange 1
#define cbAtStartOfSimTime 5
#define cbReadWriteSynch 6
#define cbReadOnlySynch 7
#define cbNextSimTime 8
#define cbAfterDelay 9
#define cbEndOfCompile 10
#define cbStartOfSimulation 11
#define cbEndOfSimulation 12

typedef struct t_vpi_time {
    PLI_INT32 type;
    PLI_UINT32 high;
    PLI_UINT32 low;
    double real;
} s_vpi_time, *p_vpi_time;

typedef struct t_vpi_vecval {
    PLI_UINT32 aval, bval;
} s_vpi_vecval, *p_vpi_vecval;

typedef struct t_vpi_strengthval {
    PLI_INT32 logic;
    PLI_INT32 s0, s1;
} s_vpi_strengthval, *p_vpi_strengthval;

typedef struct t_vpi_value {
    PLI_INT32 format;
    union {
        PLI_BYTE8 *str;
        PLI_INT32 scalar;
        PLI_INT32 integer;
        double real;
        struct t_vpi_time *time;
        struct t_vpi_vecval *vector;
        struct t_vpi_strengthval *strength;
        PLI_BYTE8 *misc;
    } value;
} s_vpi_value, *p_vpi_value;

typedef struct t_cb_data {
    PLI_INT32 reason;
    PLI_INT32 (*cb_rtn)(struct t_cb_data *);
    vpiHandle obj;
    p_vpi_time time;
    p_vpi_value value;
    PLI_INT32 index;
    PLI_BYTE8 *user_data;
} s_cb_data, *p_cb_data;

typedef struct t_vpi_error_info {
    PLI_INT32 state;
    PLI_INT32 level;
    PLI_BYTE8 *message;
    PLI_BYTE8 *product;
    PLI_BYTE8 *code;
    PLI_BYTE8 *file;
    PLI_INT32 line;
} s_vpi_error_info, *p_vpi_error_info;

typedef struct t_vpi_vlog_info {
    PLI_INT32 argc;
    PLI_BYTE8 **argv;
    PLI_BYTE8 *product;
    PLI_BYTE8 *version;
} s_vpi_vlog_info, *p_vpi_vlog_info;

vpiHandle vpi_register_cb(p_cb_data cb_data_p);
PLI_INT32 vpi_remove_cb(vpiHandle cb_obj);
vpiHandle vpi_handle_by_name(PLI_BYTE8 *name, vpiHandle scope);
vpiHandle vpi_handle_by_index(vpiHandle object, PLI_INT32 indx);
vpiHandle vpi_handle(PLI_INT32 type, vpiHandle refHandle);
vpiHandle vpi_iterate(PLI_INT32 type, vpiHandle refHandle);
vpiHandle vpi_scan(vpiHandle iterator);
PLI_INT32 vpi_get(PLI_INT32 property, vpiHandle object);
PLI_BYTE8 *vpi_get_str(PLI_INT32 property, vpiHandle object);
void vpi_get_value(vpiHandle expr, p_vpi_value value_p);
vpiHandle vpi_put_value(vpiHandle object, p_vpi_value value_p,
                        p_vpi_time time_p, PLI_INT32 flags);
void vpi_get_time(vpiHandle object, p_vpi_time time_p);
PLI_INT32 vpi_free_object(vpiHandle object);
PLI_INT32 vpi_control(PLI_INT32 operation, ...);
PLI_INT32 vpi_chk_error(p_vpi_error_info error_info_p);
PLI_INT32 vpi_get_vlog_info(p_vpi_vlog_info vlog_info_p);

#ifdef __cplusplus
}
#endif
#endif
