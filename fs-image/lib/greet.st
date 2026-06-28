// A nested example program, loaded with `:load lib/greet.st`. Proof that a file
// in a subdirectory is reachable on the metal: the recursive fs-image seed put
// it at /lib/greet.st, and the shell path-walks (lookup /lib → lookup greet.st)
// to read it. Call `greet()` after loading.
greet() uses Telemetry = emit("greeted", 1)
