// bank.st — a small ledger: `mut` accounts, capability-gated audit logging,
// and the one behavior every later program in this batch needs to know
// about before reaching for `mut`: **`mut` in Stitch is value semantics with
// automatic write-back to the call-site binding, not Java/Kotlin-style
// shared-reference mutation.**
//
// `docs/language-design.md`'s `on` section says a `mut` field "is visible
// through all aliases (Java/Kotlin semantics, not value copies)". The
// shipped interpreter does the opposite, on purpose and with its own tests
// proving it (`interp.rs::field_assignment_does_not_alias_a_copy`,
// `a_mut_method_does_not_alias_a_copy`): a `mut` method rebuilds its `@` and
// writes the result back to *the expression it was called on* — a `Var`,
// `@`, or a field path — after the call returns. Two separate bindings that
// happen to hold "the same" record at the time of the call do **not** see
// each other's mutations, because there is no shared heap cell backing
// them; each is its own copy from the moment they diverged. See
// plans/stitch-examples-findings.md for the fuller writeup.
//
// The consequence for this file: an `Account` sitting inside `Ledger`'s
// `accounts` list is not "the same object" you get back from `findAccount`
// in any aliasing sense — it's a copy. Depositing into that copy does
// nothing to the list. Every ledger-level operation below has to explicitly
// write the updated account back into `@accounts` (`replaceAccount`) after
// mutating it — the functional-update discipline `List.set` already implies,
// made unavoidable rather than optional.
//
// One more piece the design doc undersells: calling a `mut` method on a
// binding declared with plain `let` is a **runtime error** ("cannot assign
// to immutable `x` (declare it with `let mut`)"), not silently accepted —
// so every `let` that will later take a `.mutMethod()` call needs `let mut`
// from the start, same as the receiver itself needs `mut deposit`/`mut
// withdraw` in its own signature.

use Str

// --- accounts ---

contract Show {
    show() -> Str
}

prod Account(id: Str, owner: Str, mut balance: Int)

on Account {
    // Returns the updated `Account` — not because callers usually need it
    // (the write-back to `@` already updates whatever variable this was
    // called on; see `Ledger.deposit` below, which never touches the return
    // value), but because `Ledger`'s own methods *do* need it, to carry the
    // update from a local `let mut acc = …` binding into `@accounts`.
    mut deposit(amount: Int) -> Result<Account, Str> uses Telemetry {
        amount <= 0
            => Err("deposit amount must be positive")
            | {
                @balance = @balance + amount
                emit("account.deposit", amount)
                Ok(@)
            }
    }

    mut withdraw(amount: Int) -> Result<Account, Str> uses Telemetry {
        amount <= 0
            => Err("withdrawal amount must be positive")
            | (amount > @balance
                => Err("insufficient funds")
                | {
                    @balance = @balance - amount
                    emit("account.withdraw", amount)
                    Ok(@)
                })
    }
}

on Account : Show {
    show() -> Str = @owner + " (" + @id + "): $" + toStr(@balance)
}

// --- the ledger ---

sum TxKind = Deposit | Withdraw | TransferOut(Str) | TransferIn(Str)

prod Transaction(account: Str, kind: TxKind, amount: Int)

prod Ledger(mut accounts: List<Account>, mut history: List<Transaction>)

newLedger() -> Ledger = Ledger([], [])

on Ledger {
    mut openAccount(id: Str, owner: Str, opening: Int) -> Account = {
        let acc = Account(id, owner, opening)
        @accounts = concat(@accounts, [acc])
        acc
    }

    findAccount(id: Str) -> Maybe<Account> = find(@accounts, a -> a.id == id)

    mut record(tx: Transaction) { @history = concat(@history, [tx]) }

    // Depositing/withdrawing at the ledger level: find the account (a fresh
    // local binding, so `mut`-method write-back has somewhere to land),
    // mutate *that* binding, then fold the change back into the list. The
    // ledger-level `Result` never needs `acc.deposit(...)`'s own return
    // value — the write-back already updated `acc` in place.
    mut deposit(id: Str, amount: Int) -> Result<(), Str> uses Telemetry {
        let mut acc = (@findAccount(id) |> okOr("no such account: " + id))?
        acc.deposit(amount)?
        @accounts = replaceAccount(@accounts, acc)
        Ok(())
    }

    mut withdraw(id: Str, amount: Int) -> Result<(), Str> uses Telemetry {
        let mut acc = (@findAccount(id) |> okOr("no such account: " + id))?
        acc.withdraw(amount)?
        @accounts = replaceAccount(@accounts, acc)
        Ok(())
    }

    // Both accounts are checked to exist *before* anything moves — if the
    // recipient doesn't exist, the sender is never debited. (An earlier
    // draft withdrew first and deposited second; a missing recipient made
    // the withdrawn amount vanish, since nothing here can roll a mutation
    // back once it's landed. There is no transaction/rollback primitive in
    // the language, so a multi-step mutation has to be made safe by
    // ordering and up-front validation, by hand, every time.)
    mut transfer(fromId: Str, toId: Str, amount: Int) -> Result<(), Str> uses Telemetry {
        // `let _ = …` rather than two bare statements: a statement that
        // starts with `(` right after a preceding expression-statement
        // fuses with it into a call (`stmt1  (stmt2)` parses as
        // `stmt1(stmt2)` — maximal munch, see the findings doc). `let`
        // starting the line rules that out.
        let _ = (@findAccount(fromId) |> okOr("no such account: " + fromId))?
        let _ = (@findAccount(toId) |> okOr("no such account: " + toId))?
        @withdraw(fromId, amount)?
        @deposit(toId, amount)?
        @record(Transaction(fromId, TransferOut(toId), amount))
        @record(Transaction(toId, TransferIn(fromId), amount))
        emit("ledger.transfer", amount)
        Ok(())
    }

    // Note: no `uses Telemetry` here, only `FsWrite` — `@transfer` carries
    // its *own* `uses Telemetry` clause and gets that authority independent
    // of what this method declares (a named call's authority is exactly its
    // own `uses` row, never inherited from or filtered by the caller — see
    // `stitch/src/natives.rs`'s `shout()/main()` test for the same rule from
    // the refusal side). `auditLine` below needs `FsWrite` the same way.
    mut transferAudited(fromId: Str, toId: Str, amount: Int, auditHandle: Int) -> Result<(), Str> uses FsWrite {
        @transfer(fromId, toId, amount)?
        auditLine(auditHandle, fromId + "->" + toId + ":" + toStr(amount))
        Ok(())
    }
}

// The list-level half of "value semantics forces an explicit write-back":
// replace the account sharing `updated`'s id, leave every other element
// untouched.
replaceAccount(accounts: List<Account>, updated: Account) -> List<Account> =
    map(accounts, a -> a.id == updated.id => updated | a)

// The `Maybe -> Result` lift `?` needs and the prelude doesn't provide
// (only `Maybe`'s own `None`/`Some` short-circuit via the `Try` contract —
// this is what turns "absent" into a *named* failure).
okOr(m, err) = match m { Some(v) => Ok(v)  None => Err(err) }

report(ledger: Ledger) -> Str =
    ledger.accounts |> map(a -> a.show()) |> Str.join("\n")

totalBalance(ledger: Ledger) -> Int =
    ledger.accounts |> map(a -> a.balance) |> total

// Explicit-capability audit logging: `Ledger`'s own methods never declare
// `FsWrite` for their core bookkeeping (see `deposit`/`withdraw`/`transfer`
// above), so only a caller that reaches for `transferAudited` — and holds a
// file handle to hand it — pays for, and grants, that authority. The
// parameter isn't named `handle` — that's a reserved word (`handle op with
// f { … }`, the effect-handler form; see plans/stitch-examples-findings.md).
// `fileHandle` is a capability the caller must already hold (see
// `natives.rs::native_fs_write`); there is no "open this path for writing"
// native, only writing through an already-delegated handle, so a caller
// can't name an arbitrary file, only use one it was given.
auditLine(fileHandle: Int, entry: Str) -> Bool uses FsWrite = fsWrite(fileHandle, entry)

// --- tests ---

test "openAccount adds a findable account with the opening balance" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
    expect ledger.findAccount("missing") == None
}

test "two accounts built the same way are structurally equal" {
    expect Account("a1", "Alice", 100) == Account("a1", "Alice", 100)
    expect Account("a1", "Alice", 100) != Account("a1", "Alice", 101)
}

test "deposit increases the balance and is visible through findAccount" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.deposit("a1", 50)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 150))
}

test "deposit rejects a non-positive amount and leaves the balance alone" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.deposit("a1", 0) == Err("deposit amount must be positive")
    expect ledger.deposit("a1", 0 - 5) == Err("deposit amount must be positive")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
}

test "deposit to a missing account is an error naming the id" {
    let mut ledger = newLedger()
    expect ledger.deposit("ghost", 10) == Err("no such account: ghost")
}

test "withdraw decreases the balance when funds are sufficient" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.withdraw("a1", 30)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 70))
}

test "withdraw refuses insufficient funds and leaves the balance alone" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.withdraw("a1", 101) == Err("insufficient funds")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
}

test "withdraw rejects a non-positive amount" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.withdraw("a1", 0) == Err("withdrawal amount must be positive")
}

test "a mut method call writes back into its own local binding without needing the return value" {
    // The point: `acc.deposit(50)` is called for its write-back effect on
    // `acc`, and `Ledger.deposit` above never captures the `Ok(Account)` it
    // returns — this test proves that write-back actually happened, using
    // the same shape directly rather than through the ledger.
    let mut acc = Account("a1", "Alice", 100)
    acc.deposit(50)
    expect acc == Account("a1", "Alice", 150)
}

test "transfer moves the balance and records both sides of the history" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    ledger.transfer("a1", "a2", 40)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 60))
    expect ledger.findAccount("a2") == Some(Account("a2", "Bob", 50))
    expect ledger.history == [
        Transaction("a1", TransferOut("a2"), 40),
        Transaction("a2", TransferIn("a1"), 40),
    ]
}

test "transfer with insufficient funds changes neither account" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 10)
    ledger.openAccount("a2", "Bob", 10)
    expect ledger.transfer("a1", "a2", 999) == Err("insufficient funds")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 10))
    expect ledger.findAccount("a2") == Some(Account("a2", "Bob", 10))
}

test "transfer to a missing recipient never debits the sender" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.transfer("a1", "ghost", 40) == Err("no such account: ghost")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
    expect ledger.history == []
}

test "show renders owner, id, and balance" {
    expect Account("a1", "Alice", 150).show() == "Alice (a1): $150"
}

test "report joins every account's show line" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    expect report(ledger) == "Alice (a1): $100\nBob (a2): $10"
}

test "totalBalance sums every account" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    ledger.openAccount("a3", "Cy", 0)
    expect totalBalance(ledger) == 110
}

test "transferAudited moves the balance and attempts the audit write" uses FsWrite {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    // The default interpreter platform has no filesystem (`NullPlatform`,
    // see `stitch/src/platform.rs`), so `fsWrite` — and so `auditLine` —
    // returns `false` here: "no filesystem" is a normal outcome, not a
    // fault, and this test is really about the capability wiring compiling
    // and running at all, not about bytes landing anywhere.
    expect ledger.transferAudited("a1", "a2", 40, 1) == Ok(())
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 60))
    expect auditLine(1, "standalone entry") == false
}
