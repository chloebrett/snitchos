//! Kernel integration tests. Each scenario boots the kernel in QEMU,
//! reads frames off the virtio-console socket, and asserts on the
//! decoded `Frame` sequence. See `plans/kernel-integration-tests.md`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

use itest_harness::{CpuProfile, ItestLock, LockError, RunnerConfig};

use crate::qemu;

/// Per-repo location of the flake-rate baseline. Lives in repo root so
/// PR diffs surface baseline changes alongside the changes that
/// motivated them.
pub(crate) const BASELINE_PATH: &str = ".itest-baseline.toml";

/// Per-checkout integration-test lock. Lives at the repo root (with a
/// `.itest.lock` entry in `.gitignore`) so it's easy to find and
/// inspect — `cat .itest.lock` shows the PID of the current holder.
const LOCK_PATH: &str = ".itest.lock";

/// Per-checkout root for per-run history directories (tier 2 NDJSON +
/// tier 3 log copies). Each itest run creates a timestamped
/// subdirectory under here. Gitignored.
pub(crate) const HISTORY_ROOT: &str = ".itest-runs";

/// Default OTLP receiver targeted by `baseline push` (no value) and the
/// end-of-run auto-push. Matches `stack/docker-compose.yml`'s
/// Prometheus container started with `--web.enable-otlp-receiver`.
pub(crate) const DEFAULT_OTLP_ENDPOINT: &str = "http://127.0.0.1:9090/api/v1/otlp";

/// Set by the SIGINT handler. First Ctrl-C trips this to `true`; the
/// runner sees it at the next iteration boundary and writes a
/// partial baseline. Second Ctrl-C in the handler exits the process
/// directly.
static INTERRUPT: AtomicBool = AtomicBool::new(false);

fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(|| {
        // swap returns the previous value. If it was already true,
        // this is the second Ctrl-C → force-quit.
        if INTERRUPT.swap(true, Ordering::SeqCst) {
            eprintln!("\n(second Ctrl-C — force-quit; pending baseline NOT written)");
            std::process::exit(130);
        } else {
            eprintln!(
                "\n(Ctrl-C — finishing current iteration; \
                 partial baseline will be written if --update-baseline. \
                 Press Ctrl-C again to force-quit.)"
            );
        }
    });
}

/// Print a failed itest capture's frame transcript (`.itest-runs/`), so a
/// capture can be inspected without hand-parsing JSON. Defaults to the latest
/// run and the first capture in it; `--scenario`/`--tail`/`--grep` narrow it.
pub fn show(run: Option<&str>, scenario: Option<&str>, tail: Option<usize>, grep: Option<&str>) -> ExitCode {
    let dir = match run {
        Some(r) => PathBuf::from(HISTORY_ROOT).join(r),
        None => match latest_run_dir() {
            Some(d) => d,
            None => {
                eprintln!("no runs under {HISTORY_ROOT}/");
                return ExitCode::FAILURE;
            }
        },
    };
    let Some(cap_path) = find_capture(&dir, scenario) else {
        eprintln!("no .capture.json in {}", dir.display());
        return ExitCode::FAILURE;
    };
    let capture = match itest_harness::load_capture(&cap_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("load {}: {e}", cap_path.display());
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "# {} — outcome={:?}, frames_seen={}",
        cap_path.display(),
        capture.outcome,
        capture.frames_seen
    );
    let frames: Vec<&String> = capture
        .transcript
        .iter()
        .filter(|f| grep.is_none_or(|g| f.contains(g)))
        .collect();
    let start = tail.map_or(0, |n| frames.len().saturating_sub(n));
    for f in &frames[start..] {
        println!("{f}");
    }
    ExitCode::SUCCESS
}

/// The most recent run directory under [`HISTORY_ROOT`] — timestamp names sort
/// lexicographically, so the max is the latest. `None` if there are no runs.
fn latest_run_dir() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(HISTORY_ROOT)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
}

/// The first `*.capture.json` in `dir` (optionally filtered to a scenario name).
fn find_capture(dir: &Path, scenario: Option<&str>) -> Option<PathBuf> {
    let mut caps: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".capture.json") && scenario.is_none_or(|s| n.contains(s)))
        })
        .collect();
    caps.sort();
    caps.into_iter().next()
}

pub(crate) mod baseline;
mod harness;
mod matchers;
mod scenarios;
pub(crate) mod schedule;
pub(crate) mod snapshot_tree;
pub(crate) mod snemu_audit;

use itest_harness::{Scenario, ScenarioReport};

use self::harness::{Boot, View};

/// Placeholder `run` for catalog metadata. Scenarios execute via
/// [`scenario_view_fn`] + the `run_group` executor (which spawns a
/// [`Boot`] and runs each scenario against a [`View`]), never through
/// `Scenario::run`. The runner only calls `Scenario::run` when no executor
/// is configured, which never happens here — so this is unreachable.
fn unreached_run() -> Result<(), String> {
    unreachable!("xtask scenarios run via the run_group executor / scenario_view_fn, not Scenario::run")
}

/// Build the integration-test catalog from a table of rows. Emits two
/// co-generated items so they can't drift: `const SCENARIOS` (metadata for
/// the runner — the `run` field is the never-called [`unreached_run`]
/// placeholder) and `fn scenario_view_fn(name)` mapping each scenario name
/// to its `fn(&mut View)` assertion body (which the executor calls).
///
/// Row grammar: `<profile> "<name>" <fn-path> [tag, …]? {"<workload>"}? ;`
/// — `wfi` is wfi-bounded (fans out across the parallel pool), `cpu` runs
/// real guest work (a serial pass); tags feed `--tag` selection; the
/// braced workload is the `workload=` bootarg + the shared-boot grouping
/// key. (`cpu_bound` classification per plans/legacy/itest-parallel-scenarios.md.)
macro_rules! catalog {
    ( $(
        $profile:ident $name:literal $func:path
        $( [ $( $tag:ident ),* $(,)? ] )?
        $( { $wl:literal } )?
    );* $(;)? ) => {
        pub(crate) const SCENARIOS: &[Scenario] = &[ $(
            catalog!(@meta $profile $name)
                $( .tagged(&[ $( stringify!($tag) ),* ]) )?
                $( .on_workload($wl) )?
        ),* ];

        fn scenario_view_fn(name: &str) -> fn(&mut View) -> Result<(), String> {
            match name {
                $( $name => $func, )*
                other => panic!("no scenario fn registered for {other:?}"),
            }
        }
    };
    (@meta wfi $name:literal) => { Scenario::new($name, unreached_run) };
    (@meta cpu $name:literal) => { Scenario::cpu_bound($name, unreached_run) };
}

catalog! {
    // v0.13: the no-bootarg default now boots `init`, so the kernel scheduler /
    // SMP demo these exercise lives behind the explicit `demo` workload (which
    // reproduces the former default exactly). `default-boot-starts-init` below
    // covers the new default path.
    wfi "boot-reaches-heartbeat"          scenarios::boot_reaches_heartbeat         [boot]          {"init"};
    wfi "heartbeat-cadence"               scenarios::heartbeat_cadence              [boot]          {"init"};
    wfi "pre-init-order"                  scenarios::pre_init_order                 [boot]          {"init"};
    wfi "kernel-runs-at-higher-half"      scenarios::kernel_runs_at_higher_half     [boot]          {"init"};
    wfi "frame-allocator-metrics"         scenarios::frame_allocator_metrics        [frame]         {"init"};
    wfi "frame-allocator-oom"             scenarios::frame_allocator_oom            [frame, oom]    {"frame-oom"};
    wfi "kernel-heap-metrics"             scenarios::kernel_heap_metrics            [heap]          {"init"};
    wfi "sched-context-switch-smoke"      scenarios::sched_context_switch_smoke     [sched]         {"init"};
    wfi "sched-spawn-registers-thread"    scenarios::sched_spawn_registers_thread   [sched]         {"demo"};
    cpu "sched-yield-round-trips"         scenarios::sched_yield_round_trips        [sched]         {"demo"};
    wfi "sched-spans-carry-task-id"       scenarios::sched_spans_carry_task_id      [sched]         {"demo"};
    wfi "sched-context-switches-on-wire"  scenarios::sched_context_switches_on_wire [sched]         {"demo"};
    wfi "sched-span-survives-yield"       scenarios::sched_span_survives_yield      [sched]         {"demo"};
    cpu "heap-oom"                        scenarios::heap_oom                       [heap, oom]    {"heap-oom"};
    cpu "workload-cooperative-baseline"   scenarios::workload_cooperative_baseline  [workload]      {"cooperative"};
    cpu "smp-producer-consumer-correctness" scenarios::smp_producer_consumer_correctness [smp, workload] {"smp burst=256"};
    wfi "ipi-self-wakeup"                 scenarios::ipi_self_wakeup                [smp, ipi]      {"init"};
    wfi "smp-secondary-hart-boots"        scenarios::smp_secondary_hart_boots       [smp]           {"init"};
    wfi "smp-spawn-on-hart-1-runs"        scenarios::smp_spawn_on_hart_1_runs       [smp]           {"demo"};
    wfi "smp-spans-carry-hart-id"         scenarios::smp_spans_carry_hart_id        [smp]           {"demo"};
    wfi "smp-ipi-wakes-idle-hart"         scenarios::smp_ipi_wakes_idle_hart        [smp, ipi]      {"demo"};
    cpu "spawn-storm"                     scenarios::spawn_storm                    [smp, stress]   {"spawn-storm"};
    cpu "ipi-pong"                        scenarios::ipi_pong                       [smp, ipi, stress] {"ipi-pong"};
    cpu "shootdown-storm"                 scenarios::shootdown_storm                [smp, stress]   {"shootdown-storm"};
    cpu "smp-tlb-shootdown-visible"       scenarios::smp_tlb_shootdown_visible      [smp]           {"tlb-shootdown"};
    cpu "smp-ping-pong-cadence"           scenarios::smp_ping_pong_cadence          [smp, ipi]      {"ping-pong"};
    cpu "sched-task-lookup-is-o1"         scenarios::sched_task_lookup_is_o1        [sched]         {"live-tasks"};
    wfi "sched-task-exits-cleanly"        scenarios::sched_task_exits_cleanly       [sched]         {"demo"};
    wfi "task-stack-high-water"           scenarios::task_stack_high_water_reported [sched]         {"demo"};
    wfi "default-boot-starts-init"        scenarios::default_boot_starts_init       [boot];
    wfi "stack-guard-fault-detected"      scenarios::stack_guard_fault_detected     [sched]         {"stack-guard"};
    wfi "deep-overflow-reports-cleanly"   scenarios::deep_overflow_reports_cleanly  [sched]         {"stack-overflow-deep"};
    wfi "boot-stack-guard-fault-detected" scenarios::boot_stack_guard_fault_detected [sched]        {"boot-stack-guard"};
    wfi "kernel-panic-emits-frame"        scenarios::kernel_panic_emits_frame       [sched]         {"panic-now"};
    wfi "block-wake-smoke"                scenarios::block_wake_smoke               [sched]         {"block-wake"};
    wfi "ipc-message-crosses"             scenarios::ipc_message_crosses            [userspace, ipc] {"ipc"};
    wfi "ipc-trace-crosses"               scenarios::ipc_trace_crosses              [userspace, ipc] {"ipc"};
    wfi "ipc-telemetry"                   scenarios::ipc_telemetry                  [userspace, ipc] {"ipc"};
    wfi "ipc-wakeup-is-prompt"            scenarios::ipc_wakeup_is_prompt           [userspace, ipc] {"ipc"};
    wfi "rpc-round-trips"                 scenarios::rpc_round_trips                [userspace, ipc] {"ipc-rpc"};
    wfi "rpc-trace-nests"                 scenarios::rpc_trace_nests                [userspace, ipc] {"ipc-rpc"};
    wfi "rpc-telemetry"                   scenarios::rpc_telemetry                  [userspace, ipc] {"ipc-rpc"};
    wfi "rpc-reply-recv"                  scenarios::rpc_reply_recv                 [userspace, ipc] {"ipc-rpc"};
    wfi "badge-mint-mints-and-refuses"    scenarios::badge_mint_mints_and_refuses   [userspace, ipc] {"badge-mint"};
    wfi "badge-handout-transfers-cap"     scenarios::badge_handout_transfers_cap    [userspace, ipc] {"badge-handout"};
    wfi "badge-demux-distinguishes-clients" scenarios::badge_demux_distinguishes_clients [userspace, ipc] {"badge-handout"};
    wfi "badge-handout-links-derivation"  scenarios::badge_handout_links_derivation  [userspace, ipc] {"badge-handout"};
    wfi "fs-connect-mints-root"           scenarios::fs_connect_mints_root          [userspace, ipc] {"fs"};
    wfi "fs-stat-root"                    scenarios::fs_stat_root                   [userspace, ipc] {"fs"};
    wfi "fs-create-stat"                  scenarios::fs_create_then_stat            [userspace, ipc] {"fs"};
    wfi "fs-write-read"                   scenarios::fs_write_read_roundtrip        [userspace, ipc] {"fs"};
    wfi "fs-lookup-rights-gate"           scenarios::fs_lookup_rights_gate          [userspace, ipc] {"fs"};
    wfi "fs-remove"                       scenarios::fs_remove_unlinks              [userspace, ipc] {"fs"};
    wfi "fs-readdir"                      scenarios::fs_readdir_lists_entries       [userspace, ipc] {"fs"};
    wfi "fs-workload"                     scenarios::fs_workload_traces             [userspace, ipc] {"fs"};
    wfi "userspace-bad-ptr"               scenarios::userspace_bad_ptr_refused      [userspace]      {"userspace-bad-ptr"};
    wfi "userspace-custom-metric"         scenarios::userspace_custom_metric        [userspace]      {"probe"};
    wfi "probe-reports-the-timebase"      scenarios::probe_reports_the_timebase     [userspace]      {"probe"};
    wfi "span-name-not-poisonable"        scenarios::span_name_not_poisonable       [userspace]      {"probe"};
    wfi "stitch-telemetry-on-the-wire"    scenarios::stitch_telemetry_on_the_wire   [userspace, stitch] {"stitch-repl"};
    wfi "stitch-fs-loads-and-runs"        scenarios::stitch_fs_loads_and_runs       [userspace, stitch, fs] {"stitch-fs"};
    wfi "stitch-fs-loads-nested"          scenarios::stitch_fs_loads_nested         [userspace, stitch, fs] {"stitch-fs"};
    wfi "spawn-image-loads-from-fs"       scenarios::spawn_image_loads_from_fs      [userspace, spawn, fs] {"spawn-image"};
    wfi "manifest-iface-served"           scenarios::manifest_iface_served          [userspace, fs] {"manifest-iface"};
    wfi "manifest-satisfy-grants-by-name" scenarios::manifest_satisfy_grants_by_name [userspace, fs] {"manifest-satisfy"};
    wfi "manifest-satisfy-refuses-unsatisfiable" scenarios::manifest_satisfy_refuses_unsatisfiable [userspace, fs] {"manifest-satisfy"};
    wfi "manifest-satisfy-attenuates"     scenarios::manifest_satisfy_attenuates     [userspace, fs] {"manifest-satisfy"};
    wfi "stitch-reads-a-line"             scenarios::stitch_reads_a_line            [userspace, stitch] {"stitch-repl"};
    wfi "stitch-print-writes-to-console"  scenarios::stitch_print_writes_to_console [userspace, stitch] {"stitch-repl"};
    wfi "stitch-hold-lists-caps"          scenarios::stitch_hold_lists_caps         [userspace, stitch] {"stitch-repl"};
    wfi "stitch-view-reads-a-file"        scenarios::stitch_view_reads_a_file       [userspace, stitch, fs] {"stitch-fs"};
    wfi "stitch-cross-pipe-runs-a-stage"  scenarios::stitch_cross_pipe_runs_a_stage [userspace, stitch, fs] {"stitch-fs"};
    wfi "stim-edits-a-file-and-saves"     scenarios::stim_edits_a_file_and_saves    [userspace, stitch, fs] {"stitch-fs"};
    wfi "stitch-grant-revoke-capevents"   scenarios::stitch_grant_then_revoke_snitches_capevents [userspace, stitch, fs] {"stitch-fs"};
    wfi "stitch-hold-shows-endpoint-name" scenarios::stitch_hold_names_the_fs_endpoint [userspace, stitch, fs] {"stitch-fs"};
    cpu "mutex-storm"                     scenarios::mutex_storm                    [smp, stress]   {"mutex-storm"};
    cpu "virtio-storm"                    scenarios::virtio_storm                   [smp, stress]   {"virtio-storm"};
    // Userspace scenarios are wfi-bounded: `hello` exits (hart 1 falls back
    // to its idle `wfi` loop) and `faulter` faults (the kernel parks the hart
    // in `wfi`). So they fan out in the parallel pool rather than a serial pass.
    // The nine `{"userspace"}` rows all boot the *same* `hello` run — a single
    // execution that grants both caps, emits telemetry=42, opens hello.work,
    // is refused a wrong-object handle, is denied an ungranted handle, yields,
    // and exits. That shared superset is what makes them one shared-boot group.
    wfi "userspace-emits-telemetry"       scenarios::userspace_emits_telemetry     [userspace]  {"userspace"};
    wfi "userspace-cannot-touch-kernel"   scenarios::userspace_cannot_touch_kernel  [userspace]  {"userspace-fault"};
    wfi "userspace-grant-snitched"        scenarios::userspace_grant_snitched       [userspace]  {"userspace"};
    wfi "userspace-cap-denied"            scenarios::userspace_cap_denied           [userspace]  {"userspace"};
    wfi "userspace-cap-granted-event"     scenarios::userspace_cap_granted_event    [userspace]  {"userspace"};
    wfi "userspace-process-exits"         scenarios::userspace_process_exits        [userspace]  {"userspace"};
    wfi "userspace-yield-round-trips"     scenarios::userspace_yield_round_trips     [userspace]  {"userspace"};
    wfi "userspace-spansink-granted"      scenarios::userspace_spansink_granted     [userspace]  {"userspace"};
    wfi "userspace-emits-span"            scenarios::userspace_emits_span           [userspace]  {"userspace"};
    wfi "userspace-has-a-root-span"       scenarios::userspace_has_a_root_span      [userspace]  {"userspace"};
    wfi "userspace-prints"                scenarios::userspace_prints               [userspace]  {"userspace"};
    wfi "userspace-refusal-snitched"      scenarios::userspace_refusal_snitched     [userspace]  {"userspace"};
    wfi "userspace-quota-refused"         scenarios::userspace_quota_refused        [userspace]  {"userspace-span-flood"};
    cpu "two-userspace-workers-round-robin" scenarios::two_userspace_workers_round_robin [userspace] {"workers"};
    wfi "heap-grows-on-demand"            scenarios::heap_grows_on_demand           [userspace]  {"heap-grow"};
    cpu "preempt-runaway-user-task"       scenarios::preempt_runaway_user_task      [userspace]  {"user-hog"};
    cpu "preemption-telemetry"            scenarios::preemption_telemetry           [userspace]  {"user-hog"};
    cpu "syscall-hog-still-preempted"     scenarios::syscall_hog_still_preempted    [userspace]  {"syscall-hog"};
    cpu "console-echo-round-trips"        scenarios::console_echo_round_trips       [userspace]  {"console-echo"};
    cpu "spawn-delegates-to-child"        scenarios::spawn_delegates_to_child       [userspace]  {"spawn-demo"};
    cpu "spawn-transfer-links-to-parent"  scenarios::spawn_transfer_links_to_parent [userspace]  {"spawn-demo"};
    cpu "spawn-reclaims-memory"           scenarios::spawn_reclaims_memory          [userspace]  {"spawn-reap"};
    cpu "spawn-reclaims-names"            scenarios::spawn_reclaims_names           [userspace]  {"spawn-reap"};
    cpu "wait-any-reaps-exiting-child"    scenarios::wait_any_reaps_the_exiting_child [userspace] {"wait-any"};
    cpu "init-supervises-a-child"         scenarios::init_supervises_a_child        [userspace]  {"init"};
    cpu "init-brings-up-fs-server"        scenarios::init_brings_up_fs_server       [userspace]  {"init"};
    cpu "init-runs-fs-client"             scenarios::init_runs_fs_client            [userspace]  {"init"};
    cpu "supervised-regrants-caps-on-restart" scenarios::supervised_regrants_caps_on_restart [userspace] {"supervised"};
    cpu "supervised-ipc-client-cap-survives" scenarios::supervised_ipc_client_cap_survives [userspace] {"supervised-ipc"};
    cpu "supervised-shuts-down-in-reverse-dep-order" scenarios::supervised_shuts_down_in_reverse_dep_order [userspace] {"supervised-shutdown"};
    cpu "supervised-kill-stops-a-child"   scenarios::supervised_kill_stops_a_child  [userspace]  {"supervised-shutdown"};
    cpu "kill-without-a-process-cap-is-refused" scenarios::kill_without_a_process_cap_is_refused [userspace] {"kill-no-cap"};
    cpu "userspace-runs-on-hart-0"        scenarios::userspace_runs_on_hart_0       [userspace]  {"user-on-hart0"};
    cpu "cross-hart-kill-stops-a-child"   scenarios::cross_hart_kill_stops_a_child  [userspace]  {"xhart-kill"};
    cpu "supervisor-detects-and-kills-a-hung-service" scenarios::supervisor_detects_and_kills_a_hung_service [userspace] {"hung-detect"};
    cpu "endpoint-create-yields-owning-cap" scenarios::endpoint_create_yields_an_owning_cap [userspace] {"endpoint-create"};
    cpu "revoke-reclaims-a-minted-cap"    scenarios::revoke_reclaims_a_minted_cap   [userspace]  {"endpoint-create"};
    cpu "notify-signal-wakes-waiter"      scenarios::notify_signal_wakes_waiter     [userspace]  {"notify-smoke"};
    cpu "priorities-ordered-but-fair"     scenarios::priorities_ordered_but_fair    [userspace]  {"priorities"};
    cpu "viewer-reads-delegated-file"     scenarios::viewer_reads_delegated_file    [userspace]  {"view-demo"};
    cpu "shell-view-command-revokes-cap"  scenarios::shell_view_command_revokes_cap [userspace]  {"shell"};
    // Framebuffer Milestone 0 (plans/framebuffer-milestone-0.md). `ramfb` is not
    // a workload/bootarg tag like the others — it's read directly by `run_group`
    // to add `-device ramfb` to the QEMU invocation (see `Boot::spawn`).
    // `framebuffer-presents` gets its own workload so it never shares a boot
    // with a non-ramfb scenario under `--shared`.
    wfi "framebuffer-presents"            scenarios::framebuffer_presents           [display, ramfb] {"ramfb"};
    wfi "framebuffer-absent-degrades-gracefully" scenarios::framebuffer_absent_degrades_gracefully [display];
}

/// Set the process-wide failure-capture transcript depth. Call once at
/// startup, before `run`. Delegates to the harness, which reads it at
/// every `Harness::spawn`.
pub fn set_capture_level(level: itest_harness::CaptureLevel) {
    harness::set_capture_level(level);
}

/// Options for one `run` — the `itest` subcommand's flags, grouped so the
/// entry point takes a single value instead of ten positional arguments.
pub struct RunConfig {
    /// Scenario name or comma-separated list; `None` runs every scenario.
    pub name: Option<String>,
    /// Number of full passes to perform — surfaces flaky scenarios.
    pub repeat: u32,
    /// Bypass the integration-test lock (use only if it's known stale).
    pub force: bool,
    /// After the run, write each scenario's results back to the baseline
    /// (previous `current` pushed to `history`).
    pub update_baseline: bool,
    /// Abort the repeat sweep once cumulative failures reach this count.
    pub fail_fast: Option<u32>,
    /// Push the canonical baseline to OTLP when the run finishes.
    pub auto_push: bool,
    /// Parallel workers for the wfi-bounded scenario batch.
    pub jobs: u32,
    /// Parallel workers for the cpu-bound scenario batch.
    pub cpu_jobs: u32,
    /// Restrict the run to one CPU-profile class.
    pub profile_filter: Option<CpuProfile>,
    /// Scenario names to exclude (applied after `profile_filter`).
    pub skip: Vec<String>,
    /// Select scenarios carrying any of these tags (union). Mutually
    /// exclusive with `name`. Empty = no tag filtering.
    pub tags: Vec<String>,
    /// Shared-boot mode: group scenarios by `workload` and run each group
    /// against one kernel boot. `false` = separate boots (the flake gate).
    pub shared: bool,
    /// Kernel optimization regime (`low` = debug, the default; `mid`/`high` =
    /// release). Threads into both the kernel *build* and the QEMU `-kernel` path,
    /// so `--opt mid` runs the whole suite against the release kernel — the QEMU
    /// counterpart to the snemu engine's `--opt`, for catching release-codegen divergences
    /// under QEMU rather than only snemu.
    pub opt: qemu::OptLevel,
}

/// Entry point from `main`: select scenarios per `config`, run them in QEMU
/// (optionally repeating `config.repeat` times), and compare against the
/// baseline. `config.name` picks scenarios (`None` = all).
pub fn run(config: RunConfig) -> ExitCode {
    let RunConfig {
        name,
        repeat,
        force,
        update_baseline,
        fail_fast,
        auto_push,
        jobs,
        cpu_jobs,
        profile_filter,
        skip,
        tags,
        shared,
        opt,
    } = config;
    if !qemu_available() {
        eprintln!("xtask test: qemu-system-riscv64 not on PATH — skipping");
        return ExitCode::SUCCESS;
    }

    // Acquire the integration-test lock. `--force` bypasses; otherwise
    // any contender (concurrent invocation from another terminal, agent,
    // or CI job on the same checkout) gets rejected here with the
    // holder's PID. The guard is held until `run` returns.
    let _lock_guard = if force {
        None
    } else {
        match ItestLock::acquire(Path::new(LOCK_PATH)) {
            Ok(guard) => Some(guard),
            Err(LockError::AlreadyHeld { pid }) => {
                eprintln!("error: {}", LockError::AlreadyHeld { pid });
                eprintln!("       Pass --force if you know the lock is stale.");
                return ExitCode::from(2);
            }
            Err(LockError::Io(e)) => {
                eprintln!("error: failed to acquire itest lock at {LOCK_PATH}: {e}");
                return ExitCode::from(2);
            }
        }
    };

    // Warn (but don't kill) about pre-existing qemus. The lock above
    // already prevents itest-vs-itest races; the remaining concern is a
    // user's `xtask boot` / `xtask debug` / manual QEMU running in
    // parallel. We surface the situation rather than silently murder it.
    let stale = detect_stale_qemus();
    if !stale.is_empty() {
        eprintln!(
            "warning: {} stale qemu-system-riscv64 process(es) detected (pid {}).",
            stale.len(),
            stale.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
        );
        eprintln!(
            "         Probably from `xtask boot`/`xtask debug` or a manual invocation."
        );
        eprintln!(
            "         Cross-test interference is possible. Kill them manually if needed."
        );
    }

    // `--tag` and a positional name select scenarios two different ways;
    // combining them is ambiguous, so reject it rather than guess an
    // intersection-vs-union. Each invocation picks one selection mode.
    if !tags.is_empty() && name.is_some() {
        eprintln!("error: --tag cannot be combined with a positional scenario name");
        return ExitCode::from(2);
    }
    // Base selection: by name (or all) when no `--tag` is given, else
    // by tag (union over the full catalog). `--profile` / `--skip`
    // filter whatever this produces.
    let to_run: Vec<&Scenario> = if tags.is_empty() {
        match name.as_deref() {
        // One name, or a comma-separated list (`itest a,b,c`).
        // Whitespace around each name is trimmed; any unknown name is a
        // hard error — a typo shouldn't silently run a subset.
        Some(n) => {
            let mut selected = Vec::new();
            for part in n.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let Some(s) = SCENARIOS.iter().find(|s| s.name == part) else {
                    eprintln!("unknown scenario: {part}");
                    eprintln!("known: {}", SCENARIOS.iter().map(|s| s.name).collect::<Vec<_>>().join(", "));
                    return ExitCode::from(2);
                };
                selected.push(s);
            }
            if selected.is_empty() {
                eprintln!("no scenarios selected (empty list?)");
                return ExitCode::from(2);
            }
            selected
        }
        None => SCENARIOS.iter().collect(),
        }
    } else {
        let all: Vec<&Scenario> = SCENARIOS.iter().collect();
        match itest_harness::select_by_tags(&all, &tags) {
            Ok(selected) => {
                eprintln!("--tag: {} scenario(s) selected", selected.len());
                selected
            }
            Err(msg) => {
                eprintln!("{msg}");
                return ExitCode::from(2);
            }
        }
    };
    let to_run: Vec<&Scenario> = match profile_filter {
        Some(p) => {
            let label = match p {
                CpuProfile::Wfi => "wfi",
                CpuProfile::Cpu => "cpu",
            };
            let filtered: Vec<&Scenario> =
                to_run.into_iter().filter(|s| s.cpu_profile == p).collect();
            if filtered.is_empty() {
                eprintln!("--profile {label}: no scenarios match this filter");
                return ExitCode::from(2);
            }
            eprintln!("--profile {label}: {} scenarios selected", filtered.len());
            filtered
        }
        None => to_run,
    };
    let to_run: Vec<&Scenario> = if skip.is_empty() {
        to_run
    } else {
        // Warn on unknown skip names — usually a typo, and silently
        // skipping nothing would hide it.
        for s in &skip {
            if !SCENARIOS.iter().any(|sc| sc.name == s.as_str()) {
                eprintln!("warning: --skip {s:?}: no such scenario (ignored)");
            }
        }
        let before = to_run.len();
        let filtered: Vec<&Scenario> = to_run
            .into_iter()
            .filter(|sc| !skip.iter().any(|s| s == sc.name))
            .collect();
        eprintln!("--skip: excluded {} scenario(s)", before - filtered.len());
        if filtered.is_empty() {
            eprintln!("--skip: all selected scenarios were excluded; nothing to run");
            return ExitCode::from(2);
        }
        filtered
    };

    // Hook closures. None of these escape the call — the lifetime
    // parameter on RunnerConfig keeps them bounded to this scope.
    // One build for the whole suite: the `itest-workloads` kernel.
    // With no `workload=` bootarg it runs the exact default demo
    // (additive guarantee), so default-demo scenarios use it as-is;
    // workload scenarios select via QEMU `-append`. No per-scenario
    // rebuilds. See `docs/runtime-workload-selection-design.md`.
    let build = || match qemu::build_kernel_profiled(&["itest-workloads"], opt) {
        // A non-zero exit (e.g. a compile error) MUST abort the run —
        // otherwise the suite silently runs the previously-built (stale)
        // kernel and reports bogus pass/fail. `map(|_| ())` used to drop
        // the status and hide exactly that.
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("kernel build failed ({status})")),
        Err(e) => Err(e.to_string()),
    };
    let commit_for = current_commit_short;

    // The executor: spawn one `Boot` for the group (separate mode = one
    // scenario; shared mode = the same-workload group), then run each
    // scenario's `fn(&mut View)` against a fresh `View` over that boot and
    // package the View's `max_wait()` / `take_capture()` + the boot's log
    // path into a `ScenarioReport`. All scenarios in a group share a
    // workload (the runner's grouping invariant), so the group's bootarg
    // is the first member's — same for the `ramfb` tag, which is why
    // `framebuffer-presents` gets its own unshared `{"ramfb"}` workload
    // (a group containing a mix of ramfb/non-ramfb members would apply
    // the device to all-or-none based on whichever member sorts first).
    let run_group = |scns: &[&Scenario]| -> Vec<ScenarioReport> {
        let Some(first) = scns.first() else { return Vec::new() };
        let ramfb = first.tags.contains(&"ramfb");
        let boot = match Boot::spawn(first.name, first.workload, ramfb, opt) {
            Ok(boot) => boot,
            // Spawn failure is infra, not a scenario assertion: report it
            // for every member so the runner records each as failed.
            Err(e) => {
                return scns
                    .iter()
                    .map(|_| ScenarioReport {
                        result: Err(format!("boot spawn failed: {e}")),
                        max_wait: None,
                        capture: None,
                        log_path: None,
                    })
                    .collect();
            }
        };
        let log_path = boot.log_path();
        scns.iter()
            .map(|s| {
                let mut view = boot.view();
                let result = scenario_view_fn(s.name)(&mut view);
                ScenarioReport {
                    result,
                    max_wait: Some(view.max_wait()),
                    capture: view.take_capture(),
                    log_path: Some(log_path.clone()),
                }
            })
            .collect()
    };

    // Install the SIGINT handler before constructing config — the
    // INTERRUPT flag is what the runner reads at iteration boundaries.
    install_ctrlc_handler();

    let config = RunnerConfig {
        one_shot_build: Some(&build),
        run_group: Some(&run_group),
        current_commit: Some(&commit_for),
        baseline_file: Some(PathBuf::from(BASELINE_PATH)),
        fail_fast,
        pending_baseline: Some(PathBuf::from(format!("{BASELINE_PATH}.pending"))),
        interrupt: Some(&INTERRUPT),
        history_root: Some(PathBuf::from(HISTORY_ROOT)),
        jobs,
        cpu_jobs,
        invocation: Some(std::env::args().collect::<Vec<_>>().join(" ")),
        shared,
    };

    let outcome = itest_harness::run(&to_run, repeat, update_baseline, &config);

    if auto_push {
        try_auto_push();
    }

    outcome
}

/// Probe the bundled stack's OTLP receiver and push the canonical
/// baseline if it answers. Warn (don't be silent) when it doesn't —
/// the user opted in to live metrics by enabling auto-push (it's on
/// by default), so silent skipping would hide the misconfiguration.
///
/// Bounded by a short connect timeout so a missing stack costs ~1s
/// at the end of a run, not ureq's default ~75s.
fn try_auto_push() {
    let endpoint = DEFAULT_OTLP_ENDPOINT;
    let timeouts = Some((
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(3),
    ));
    match baseline::load_and_push(endpoint, timeouts) {
        // No baseline yet (e.g. first run on a fresh checkout) — silent skip.
        Ok(None) => {}
        Ok(Some((status, scenarios))) if (200..300).contains(&status) => {
            eprintln!("auto-push: pushed {scenarios} scenarios to {endpoint} (HTTP {status})");
        }
        Ok(Some((status, _))) => {
            eprintln!(
                "auto-push: OTLP receiver at {endpoint} returned HTTP {status}.\n\
                 Confirm the stack is healthy, or pass --no-auto-push to silence."
            );
        }
        Err(e) => {
            eprintln!(
                "auto-push: skipped ({e}).\n\
                 Run `cargo xtask stack up`, or pass --no-auto-push to silence."
            );
        }
    }
}

/// Returns the short commit hash for HEAD via `git rev-parse`. None on
/// any failure (not in a git checkout, git missing, etc.). The baseline
/// file falls back to "unknown" in that case.
fn current_commit_short() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Crates the host gate deliberately does **not** `cargo test`, each with the
/// reason. Every other workspace member is tested by default: the gate derives
/// its list from `cargo metadata`, so a new crate is host-tested the moment it
/// joins the workspace, and this list is the only way out. Opting out is a
/// decision someone has to write down.
///
/// The inverse (an allow-list) let a crate be silently never-tested by simple
/// omission — which is exactly how the `kernel-core` rename slipped through.
pub(crate) const NOT_HOST_TESTED: &[(&str, &str)] = &[
    ("kernel", "no_std/no_main, riscv64-only — won't link for the host; its logic lives in the kernel-* crates"),
    ("snitchos-user", "riscv64-only userspace runtime (crt0 + syscall bindings)"),
    ("snitchos-std", "riscv64-only userspace std"),
    ("hello", "riscv64-only userspace binaries"),
    ("fs", "riscv64-only userspace FS server"),
];

/// Extra `cargo test` args a crate's suite needs (features it can't get from
/// its defaults). Entries naming a departed crate are an error, not a no-op.
pub(crate) const EXTRA_TEST_ARGS: &[(&str, &[&str])] = &[
    // `protocol::stream` (decoder + OwnedFrame) is behind `std`.
    ("protocol", &["--features", "std"]),
    // `--features testing` exposes `stitch::testing` so the integration tests
    // (e.g. the stim FSM in `tests/stim_fsm.rs`) can run the interpreter.
    ("stitch", &["--features", "testing"]),
];

/// The crates the riscv gate lints: exactly the [`NOT_HOST_TESTED`] set, in
/// member order. The two lists coincide for one reason — a crate is exempt from
/// the host gate *because* it only builds for riscv64 — so that one axis decides
/// both gates rather than each keeping its own list to drift.
///
/// A stale entry is an error, for the same reason it is in [`unit_test_plan`].
pub(crate) fn riscv_only_plan<'a>(
    members: &[&'a str],
    riscv_only: &[(&str, &str)],
) -> Result<Vec<&'a str>, String> {
    let stale: Vec<&str> =
        riscv_only.iter().map(|(name, _)| *name).filter(|name| !members.contains(name)).collect();
    if !stale.is_empty() {
        return Err(format!(
            "riscv-only policy names crates that are not workspace members: {}. \
             Renamed or removed? Update NOT_HOST_TESTED in xtask/src/itest.rs.",
            stale.join(", ")
        ));
    }

    Ok(members
        .iter()
        .filter(|name| riscv_only.iter().any(|(excluded, _)| excluded == *name))
        .copied()
        .collect())
}

/// The workspace's package names, straight from `cargo metadata --no-deps`.
pub(crate) fn workspace_members() -> Result<Vec<String>, String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|e| format!("run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err("cargo metadata failed".to_string());
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("parse cargo metadata: {e}"))?;
    let packages = json["packages"].as_array().ok_or("cargo metadata: no packages array")?;
    Ok(packages.iter().filter_map(|p| p["name"].as_str().map(str::to_owned)).collect())
}

/// Decide which crates the host gate tests, and with what args: every member
/// that isn't excluded, in member order. Both lists must describe crates that
/// actually exist — a stale entry (renamed or deleted crate) is an error, so a
/// rename can't quietly drop a crate out of the gate.
pub(crate) fn unit_test_plan<'a>(
    members: &[&'a str],
    excluded: &[(&str, &str)],
    extra_args: &[(&'static str, &'static [&'static str])],
) -> Result<Vec<(&'a str, &'static [&'static str])>, String> {
    let stale: Vec<&str> = excluded
        .iter()
        .map(|(name, _)| *name)
        .chain(extra_args.iter().map(|(name, _)| *name))
        .filter(|name| !members.contains(name))
        .collect();
    if !stale.is_empty() {
        return Err(format!(
            "unit-test policy names crates that are not workspace members: {}. \
             Renamed or removed? Update NOT_HOST_TESTED / EXTRA_TEST_ARGS in xtask/src/itest.rs.",
            stale.join(", ")
        ));
    }

    Ok(members
        .iter()
        .filter(|name| !excluded.iter().any(|(excluded, _)| excluded == *name))
        .map(|name| {
            let args = extra_args
                .iter()
                .find(|(crate_name, _)| crate_name == name)
                .map_or(&[] as &'static [&'static str], |(_, args)| *args);
            (*name, args)
        })
        .collect())
}

/// Run every host-side check, in order: each workspace crate's unit tests, the
/// loom model-checks, and the generated-diagram drift check. Returns `SUCCESS`
/// only if all pass. Bails out on first failure (no point continuing if a
/// foundation crate is broken).
/// Width of the [`progress_bar`] gauge, in characters (excluding the brackets).
const BAR_WIDTH: usize = 20;

/// An ASCII completion gauge: `[#####...............]`. Truncating division means
/// a bar only reads full at `done == total`, so "full" always means finished.
fn progress_bar(done: usize, total: usize, width: usize) -> String {
    let filled = if total == 0 { 0 } else { done * width / total };
    format!("[{}{}]", "#".repeat(filled), ".".repeat(width - filled))
}

pub fn run_unit_tests() -> ExitCode {
    let members = match workspace_members() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("unit tests: {e}");
            return ExitCode::from(1);
        }
    };
    let names: Vec<&str> = members.iter().map(String::as_str).collect();
    let plan = match unit_test_plan(&names, NOT_HOST_TESTED, EXTRA_TEST_ARGS) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("unit tests: {e}");
            return ExitCode::from(1);
        }
    };
    // +1 for the loom model-check below: the gauge counts every cargo suite the
    // section runs, so it reaches full exactly when the section is done.
    let total = plan.len() + 1;
    eprintln!("=== unit tests ({total} suites) ===");
    for (done, (crate_name, extra_args)) in plan.iter().enumerate() {
        let mut args = vec!["test", "-p", *crate_name, "--quiet"];
        args.extend_from_slice(extra_args);
        let label = format!("{} {:>2}/{total} {crate_name}", progress_bar(done, total, BAR_WIDTH), done + 1);
        if !run_cargo_test(&label, &args, &[]) {
            return ExitCode::from(1);
        }
    }
    // The loom model-check tests (kernel-devices/tests/loom_tx.rs) live
    // behind `--cfg loom`, where loom swaps in its own Mutex/thread/
    // UnsafeCell. They need a separate compilation with that cfg set; a
    // normal `cargo test` compiles the file to nothing. The config-level
    // rustflags are riscv-target-scoped, so overriding RUSTFLAGS for this
    // host build clobbers nothing.
    let label =
        format!("{} {total:>2}/{total} kernel-devices (loom)", progress_bar(total - 1, total, BAR_WIDTH));
    if !run_cargo_test(
        &label,
        &["test", "-p", "kernel-devices", "--test", "loom_tx", "--quiet"],
        &[("RUSTFLAGS", "--cfg loom")],
    ) {
        return ExitCode::from(1);
    }
    // Generated diagrams (docs/generated/) are contract artifacts: a stale one
    // means the source of truth moved without the committed diagram noticing.
    // `--check` re-derives and diffs, failing the gate on drift.
    eprintln!("=== generated diagrams ===");
    if crate::diagram_cmd::check_all() != ExitCode::SUCCESS {
        return ExitCode::from(1);
    }
    // Same reasoning as the diagrams: a markdown link is a contract nothing
    // compiles. Every `git mv` sweep this repo has done has left dead links
    // behind — most often the moved file's own `../` links, which now resolve
    // one directory too high. Cheap to check, invisible otherwise.
    eprintln!("=== doc links ===");
    if crate::links::check() != ExitCode::SUCCESS {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// Run one `cargo test` invocation, printing `ok`/`FAILED` for `label`.
/// On failure surfaces the captured stderr so the user needn't re-run
/// with `--verbose`. `env` overrides are applied to the child (e.g.
/// `RUSTFLAGS=--cfg loom`). Returns `true` iff the suite passed.
fn run_cargo_test(label: &str, args: &[&str], env: &[(&str, &str)]) -> bool {
    // Cargo's stderr is inherited, not captured: compiling is what the wall-clock
    // is actually spent on (suites *run* in ~0-3s), so cargo's own `Compiling …`
    // progress is the live signal. Capturing it to replay only on failure is what
    // made the gate look hung for minutes at a time. The label goes on its own
    // line so cargo's output streams beneath it rather than trailing it.
    eprintln!("  {label}");
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit());
    for (key, value) in env {
        cmd.env(key, value);
    }
    match cmd.status() {
        Ok(status) if status.success() => {
            eprintln!("    ok");
            true
        }
        // Cargo already streamed the failure to the terminal — don't replay it.
        Ok(_) => {
            eprintln!("    FAILED");
            false
        }
        Err(e) => {
            eprintln!("    FAILED to invoke cargo: {e}");
            false
        }
    }
}

fn qemu_available() -> bool {
    std::process::Command::new("qemu-system-riscv64")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Wipe out any `qemu-system-riscv64` processes already on the host.
/// Run before the suite by default — a stale QEMU from `cargo xtask
/// boot`, a debug session, or a previous interrupted suite would
/// compete for host CPU and cause spurious flakes. Bypassed with
/// `--keep-existing-qemus`.
/// Return the PIDs of currently-running `qemu-system-riscv64`
/// processes. The integration-test lock prevents itest-vs-itest races
/// directly; this detection covers the remaining case of `xtask boot`,
/// `xtask debug`, or a manually-launched QEMU sharing the machine.
fn detect_stale_qemus() -> Vec<u32> {
    std::process::Command::new("pgrep")
        .arg("qemu-system-riscv64")
        .output()
        .ok()
        .map(|o| {
            std::str::from_utf8(&o.stdout)
                .map(|s| s.lines().filter_map(|l| l.trim().parse().ok()).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod progress_tests {
    use super::progress_bar;

    #[test]
    fn an_unstarted_run_shows_an_empty_bar() {
        assert_eq!(progress_bar(0, 4, 8), "[........]");
    }

    #[test]
    fn a_finished_run_shows_a_full_bar() {
        assert_eq!(progress_bar(4, 4, 8), "[########]");
    }

    #[test]
    fn progress_fills_proportionally() {
        assert_eq!(progress_bar(2, 4, 8), "[####....]");
        assert_eq!(progress_bar(1, 4, 8), "[##......]");
    }

    #[test]
    fn the_bar_is_always_its_stated_width() {
        for done in 0..=7 {
            assert_eq!(progress_bar(done, 7, 12).len(), 14, "done={done}");
        }
    }

    /// Partial progress must never render as finished — a full bar means done.
    #[test]
    fn progress_short_of_the_end_never_looks_full() {
        assert_eq!(progress_bar(6, 7, 12), "[##########..]");
    }

    #[test]
    fn an_empty_plan_does_not_divide_by_zero() {
        assert_eq!(progress_bar(0, 0, 4), "[....]");
    }
}

#[cfg(test)]
mod unit_test_plan_tests {
    use super::{EXTRA_TEST_ARGS, NOT_HOST_TESTED, unit_test_plan};

    #[test]
    fn a_crate_nobody_mentions_is_tested_by_default() {
        let plan = unit_test_plan(&["brand-new-crate"], &[], &[]).expect("valid plan");
        assert_eq!(plan, vec![("brand-new-crate", &[] as &[&str])]);
    }

    #[test]
    fn an_excluded_crate_is_skipped() {
        let plan = unit_test_plan(&["kernel", "collector"], &[("kernel", "riscv only")], &[])
            .expect("valid plan");
        assert_eq!(plan, vec![("collector", &[] as &[&str])]);
    }

    #[test]
    fn extra_args_attach_to_their_crate() {
        let plan = unit_test_plan(&["protocol"], &[], &[("protocol", &["--features", "std"])])
            .expect("valid plan");
        assert_eq!(plan, vec![("protocol", &["--features", "std"] as &[&str])]);
    }

    #[test]
    fn plan_follows_member_order() {
        let plan = unit_test_plan(&["b", "a"], &[], &[]).expect("valid plan");
        assert_eq!(plan.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec!["b", "a"]);
    }

    /// A renamed or deleted crate must not leave a silent entry behind — that is
    /// how `kernel-core`'s rename slipped past the gate.
    #[test]
    fn an_exclusion_naming_a_departed_crate_is_an_error() {
        let err = unit_test_plan(&["collector"], &[("kernel-core", "gone")], &[])
            .expect_err("stale exclusion must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    #[test]
    fn extra_args_naming_a_departed_crate_are_an_error() {
        let err = unit_test_plan(&["collector"], &[], &[("kernel-core", &["--features", "std"])])
            .expect_err("stale args must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    /// The committed lists describe the real workspace, checked against the real
    /// `cargo metadata` — this is what fails when a crate is renamed or removed.
    #[test]
    fn the_committed_lists_match_the_workspace() {
        let members = super::workspace_members().expect("cargo metadata");
        let names: Vec<&str> = members.iter().map(String::as_str).collect();
        unit_test_plan(&names, NOT_HOST_TESTED, EXTRA_TEST_ARGS).expect("committed lists are current");
    }
}

#[cfg(test)]
mod riscv_only_plan_tests {
    use super::{NOT_HOST_TESTED, riscv_only_plan, unit_test_plan};

    #[test]
    fn returns_the_excluded_crates_in_member_order() {
        let plan = riscv_only_plan(&["collector", "kernel", "hello"], &[
            ("hello", "riscv only"),
            ("kernel", "riscv only"),
        ])
        .expect("valid plan");
        assert_eq!(plan, vec!["kernel", "hello"]);
    }

    #[test]
    fn a_host_crate_is_not_in_the_riscv_plan() {
        let plan = riscv_only_plan(&["collector"], &[]).expect("valid plan");
        assert!(plan.is_empty(), "no exclusions means nothing is riscv-only: {plan:?}");
    }

    /// Same guard the unit-test plan has: a renamed crate must not leave a
    /// silent entry behind.
    #[test]
    fn an_entry_naming_a_departed_crate_is_an_error() {
        let err = riscv_only_plan(&["collector"], &[("kernel-core", "gone")])
            .expect_err("stale entry must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    /// The invariant the hardcoded clippy allow-list used to break: every
    /// workspace member is linted by exactly one of the two gates. A crate can
    /// no longer be silently unlinted by simple omission — which is how
    /// `snemu`, `stitch`, `hitch` and eleven others drifted out.
    #[test]
    fn every_workspace_member_is_linted_by_exactly_one_gate() {
        let members = super::workspace_members().expect("cargo metadata");
        let names: Vec<&str> = members.iter().map(String::as_str).collect();
        let host: Vec<&str> = unit_test_plan(&names, NOT_HOST_TESTED, &[])
            .expect("valid plan")
            .iter()
            .map(|(name, _)| *name)
            .collect();
        let riscv = riscv_only_plan(&names, NOT_HOST_TESTED).expect("valid plan");

        for member in &names {
            let in_host = host.contains(member);
            let in_riscv = riscv.contains(member);
            assert!(in_host || in_riscv, "{member} is linted by neither gate");
            assert!(!(in_host && in_riscv), "{member} is linted by both gates");
        }
    }
}
