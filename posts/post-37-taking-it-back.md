# Post 37 — Taking it back

- the powerbox had three of its four verbs. you could **grant** authority (hand a child exactly the caps its job needs), the child could **use** it, and you could **watch** every grant land on the wire as a `CapEvent`. what you couldn't do was **take it back**. authority went out and never came home. a capability OS where grants are one-way isn't a powerbox — it's a generous-once handout. this is the post where the loop closes: a process can reclaim what it delegated, the reclaim is transitive across the whole subtree it gave away, and — because it's SnitchOS — you watch the authority drain back out.

- it's prework for the shell. the shell's whole pitch is "grant a program exactly what this command needs, run it, watch what it touched." that story has a fourth beat — *and then take the authority back when the command's done* — and without revocation the shell would be a powerbox that only ever leaks. so before the shell, the kernel learns to reclaim.

## the obvious fix is wrong

- my first instinct was: a handle already carries a generation. revoking is just bumping the slot's generation — every stale handle to it then fails the check. done. and that's real, and it's the wrong tool, because of *where the grant lives*.

- when a process delegates a cap, the grantee doesn't get a reference to my holding — it gets a **copy** in its own table, its own slot, its own generation. bumping the generation on *my* slot invalidates *my* handle and does precisely nothing to the copy I handed out. the cap I want to reclaim isn't in my table at all. so "revoke a handle" is the wrong verb. the right one is "revoke a *holding*, wherever it ended up" — and that means naming caps by something stable across the copy, not by the integer that addresses one slot in one table.

- which is exactly the thing post 35 built and I almost didn't realise I'd need again. every holding already carries a global `cap_id` — the identity I added so the delegation *tree* could be drawn. it turns out the id you need to *observe* a delegation is the same id you need to *undo* one. revocation is keyed on `cap_id`: find the holding with this id, in whatever process holds it, and invalidate it.

## the tree I built to watch became the tree I walk to reclaim

- here's the part that made me grin. revoking one cap isn't enough. say the shell grants a file cap to a program, and that program passes it along to a helper it spawned. reclaiming the shell's grant has to reach the helper's copy too, or the authority didn't actually come back — it just moved one hop further away. revocation has to be **transitive**: revoke a holding and you revoke everything derived from it, all the way down.

- and "everything derived from it" is a phrase I already had a data structure for. post 35 gave every holding a `parent_cap_id` — the edge pointing at the holding it was copied from. I built those edges so Tempo could *draw* the delegation graph. reclaiming a subtree is just walking those same edges the other direction: start at the revoked cap, find every holding whose parent is it, then every holding whose parent is *those*, and invalidate the lot. the structure I added for observability turned out to be the enforcement mechanism. I didn't build a second thing. the snitching graph *is* the revocation graph.

- the walk is a little fixpoint across every process's capability table: take a node, sweep its direct children wherever they live, kill each and add it to the frontier, repeat until the frontier drains. it terminates for a reason that's quietly satisfying — a child's `cap_id` is always minted *after* its parent's, so it's always larger, so the parent→child edges can't form a cycle. the tree is a tree by construction. and the caller's own holding is the root of the walk, never swept: you reclaim what you gave, you keep what's yours.

## the authority to revoke is just holding the cap

- there's a question lurking here that every access-control system has to answer: *who's allowed to revoke?* and the capability answer is so clean it almost looks like a missing feature. you revoke by naming a cap **you hold a handle to**. resolving that handle in your own table — proving you hold the cap — *is* the authorisation. holding a capability is, definitionally, the right to reclaim what was derived from it. there's no access-control list, no "owner" field, no second check asking whether you're allowed. the syscall takes a handle; if it resolves, you're allowed; the thing it reclaims is its descendants.

- I keep relearning this lesson and it keeps being the point: in a capability system you don't add a permission check, you *already have one* — it's the cap. the revoke syscall has no policy in it. it has a handle resolve, a tree walk, and a counter.

## the fourth verb is observable too

- revocation emits a `CapEvent` like every other authority move — `Revoked`, carrying the cap's id and the process it was taken from, one per holding swept. so the lifecycle is now complete *on the wire*: `Granted` when authority is born, `Transferred` when it's delegated, `Revoked` when it's reclaimed. you can watch a file cap get handed to a program, watch the program use it, and watch it wink out of the program's table the moment the grant is pulled — same channel, same graph, the edges lighting up forward on delegation and the nodes going dark on reclaim.

- the end-to-end test is small and says the whole thing: a program makes an endpoint, mints a badged `send` cap from it, then revokes. the trace shows the mint as a `Transferred` edge off the endpoint, then a `Revoked` event pointing at that same parent — the minted child reclaimed, the count coming back exactly one. proof, on the wire, that authority went out and came home.

## what I learned

- **revoke by identity, not by handle.** the handle addresses one slot in one table; the grant you want to reclaim is a copy in *someone else's* table. you can't reach it by handle — you reach it by the stable `cap_id` that survives the copy. the integer that's convenient for *using* a cap is the wrong key for *reclaiming* one.

- **the observability substrate was the enforcement substrate.** I built `cap_id` and `parent_cap_id` to make delegation *visible*. transitive revocation needs to find a holding by id and walk its descendants — which is the exact same index and the exact same edges. the graph I drew for the trace is the graph I walk to revoke. SnitchOS's one habit — emit the invisible thing — keeps turning out to have built the next feature already.

- **doing the prework first made the hard version free.** I split this into "store the parent edges" then "walk them," and did the storing first as its own step. by the time I wrote the syscall, transitive revocation wasn't extra work — the walk was just *there* to write, because the data it needed already existed and was already tested. the boring increment paid for the interesting one.

- **authorisation you don't have to write.** the scariest-sounding part — "who may revoke what?" — has no code. holding the cap is the right. every time I reach for an access check in this project, the capability turns out to already be the check.

## what's next

- one verb left to surface, and it's not in the kernel — it's in the shell. the `revoke` machinery is done: the syscall, the transitive walk, the `Revoked` event, the userspace binding. what's missing is the *word* — a shell that, when a command finishes, reclaims the caps it lent and shows you the authority coming back. that lands when the shell does, and the shell is a Stitch program, because "the platform provides the effects and you watch them" is the same sentence whether you're talking about a capability OS or a language with `uses`.

- so the powerbox is whole now, in primitives if not yet in prose: grant, use, watch, **reclaim**. you can hand out a slice of your world and you can take it back, and both directions are a graph you can point at. the shell is the thing that finally lets a *person* do it — but the OS underneath it can now do the one thing a powerbox has to: not just give, but un-give.
