// Context switch: save `from`'s callee-saved register set, load
// `to`'s, return. To both threads this looks like a regular function
// call that took a long time.
//
//   void switch(TaskContext *from /* a0 */, TaskContext *to /* a1 */);
//
// TaskContext layout (kernel/src/sched.rs) — order is load-bearing,
// matches the offsets below:
//   0   ra
//   8   sp
//   16  s0    (+8 each up to)
//   24  s1
//   32  s2
//   40  s3
//   48  s4
//   56  s5
//   64  s6
//   72  s7
//   80  s8
//   88  s9
//   96  s10
//   104 s11
//
// Caller-saved regs (a0-a7, t0-t6, ft0-ft11, fa0-fa7) are not
// preserved by the switch — the C ABI lets the compiler treat them
// as clobbered across a call, and `switch` IS a call from both
// threads' perspective.

.section .text
.global switch
.align 2
switch:
    sd ra,    0(a0)
    sd sp,    8(a0)
    sd s0,   16(a0)
    sd s1,   24(a0)
    sd s2,   32(a0)
    sd s3,   40(a0)
    sd s4,   48(a0)
    sd s5,   56(a0)
    sd s6,   64(a0)
    sd s7,   72(a0)
    sd s8,   80(a0)
    sd s9,   88(a0)
    sd s10,  96(a0)
    sd s11, 104(a0)

    ld ra,    0(a1)
    ld sp,    8(a1)
    ld s0,   16(a1)
    ld s1,   24(a1)
    ld s2,   32(a1)
    ld s3,   40(a1)
    ld s4,   48(a1)
    ld s5,   56(a1)
    ld s6,   64(a1)
    ld s7,   72(a1)
    ld s8,   80(a1)
    ld s9,   88(a1)
    ld s10,  96(a1)
    ld s11, 104(a1)
    ret

// Load-only sibling of `switch`. Loads the callee-saved register set
// from *to (a0) and `ret`s into the resumed thread. The current
// thread's registers are NOT saved — caller is committing to never
// being resumed.
//
//   void switch_into(TaskContext *to /* a0 */) -> !;
//
// Used by `sched::exit_now`. After this runs, the caller's stack and
// `TaskContext` may be freed at any later time (today: leaked).

.section .text
.global switch_into
.align 2
switch_into:
    ld ra,    0(a0)
    ld sp,    8(a0)
    ld s0,   16(a0)
    ld s1,   24(a0)
    ld s2,   32(a0)
    ld s3,   40(a0)
    ld s4,   48(a0)
    ld s5,   56(a0)
    ld s6,   64(a0)
    ld s7,   72(a0)
    ld s8,   80(a0)
    ld s9,   88(a0)
    ld s10,  96(a0)
    ld s11, 104(a0)
    ret
