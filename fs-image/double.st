// A tiny `~>` stage: doubles its Int input. It emits the result as `pipe.out`
// so an integration test can observe the pipeline running end-to-end on the
// metal (the cross-process pipe's typecheck-and-run path). Its `main`
// signature — `main(x: Int) -> Int uses Telemetry` — *is* its typed-process
// manifest: input Int, output Int, uses Telemetry.
main(x: Int) -> Int uses Telemetry = {
    let d = x + x
    emit("pipe.out", d)
    d
}
