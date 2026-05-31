- told claude to build scaffold - it overdid it and wrote the linker too.
- I made a revert commit, and then stepped through step by step.
- pretty much implemented this: https://os.phil-opp.com/freestanding-rust-binary/

![i-am-alive.png](i-am-alive.png)

- ecalling into machine mode to print chars one at a time.
- ran on qemu!
- looping, "wait for interrupt", setting memory to zero
- learned a bit about the linker script format
- compiling for risc-v
- "i am alive"
