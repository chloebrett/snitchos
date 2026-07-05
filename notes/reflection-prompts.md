# Reflection prompts

Reusable prompts for stepping back and eliciting a high-level project review from a
high-altitude model (Fable-tier), away from implementation. Captured 2026-07-05 from
the session that produced the first such reflection.

The personal / dated specifics — reading list, "around 5 weeks in", the 40/40/20
split, the file paths — are yours to update per use. The _shape_ is the reusable
part: state your profile and lens, the weighted goals, the artifact to look at, then
ask a pointed "how am I doing against these, and how could I do better" with an
explicit invitation to range from small tweaks to whole new philosophies, plus the
caveat about what you're optimizing for (here: enjoyment weighed against artifact /
learning / agent-loop quality).

Reproduced verbatim (typos and all).

---

## 1. The opening prompt (the reusable template)

```
what do you think of this project, from the perspective of a senior Google
engineer (my profile is: typically working on Android and server features; strong
focus on maintainability, code health and documentation) growing into a more
systems/platform focus as a side project. I've read OSTEP, Linux Kernel
Development, done half of nand2tetris (this is, in a way, the other half turned up
to 11), and many other books. The three angles I am approaching this project with
are:

40%: learning vehicle to improve my technical skills and also as a secondary
effect coordination of a large project. The "learning" dir is for direct technical
learning where otherwise the LLM may take away some of this as a trade for
velocity.
40%: demonstrable artifact via the end product, strong documentation, draft posts
(LLM written for now) which may be turned into an article and/or YouTube series.
20%: opportunity for me to experiment with agentic development in a low-stakes
environment; the challenge is balancing understanding against velocity and
managing to keep the project in my head as well as understandable by the LLM. I'm
mostly using Opus 4.8; occasional splashes of Fable like now for high level
guidance and reflection.

See also: snitchos/docs/README.md

So the actual question I am asking: how would you say I've done so far (around 5
weeks in) at sticking to these goals, covering interesting and sometimes novel
ground, and flexing my muscles as a senior engineer? How can I do better at this?
I'm open to ideas ranging from small tweaks to the approach to entirely new
philophies. One caveat is that I'm greatly enjoying the project as it currently
stands and the meta-loop of development; enjoyment should be a factor in potential
other considerations for how to approach - but it can also be weighed up against
the quality of the artifact, the learning, and the agent loop.
```

---

## 2. The point-by-point pushback (reactive — for reference)

This one responded to the model's specific assessment rather than standing alone as
a prompt, but it captures reusable framings worth keeping: the attribution stance
(the prose is Opus's, shaped but not written by you), focus inertia, deliberate
design fan-out as an agent-concurrency structure, the output-side evidence for
comprehension (DAW-from-scratch, hand-implemented Stitch dispatch, toy Sv39 walker,
self-quizzing), and the positioning claim (no toy OS combines caps + serious
observability + typed data + an elegant shell). Verbatim:

```
"this is the judgment that distinguishes staff-level work from competent
implementation." appreciate the complement, but here we're talking about a
collaboration between myself and Opus. Opus wrote the vast majority (95%+) of the
markdown you see. I've shaped it significantly, but the final draft is not my
writing.

"The artifact goal has received ~0% of its last mile." You're correct, and I'm
aware of it. The reality is about my focus: switching gear to the videos, the
articles etc is a complete change of focus, and I have a lot of focus inertia as a
person. I'm "locked in" to the design and implementation side right now. I don't
intend to publish 42 posts of LLM drivel (not that it's that bad anyway - but I
wouldn't put my name on it) to the public. But the posts are a hedge so that I
don't forget what I did; I'm going to dig up the commit log, the post history, make
a bunch of interesting demos, and then the videos and real articles come from that.
But it is, in a way, a different project. If I check in in two months and still
haven't done any of that, then a reprioritization is on the cards.

"Design output is outpacing build ~5:1" This is okay, and also intentional. Agents
make implementation cheap; they also make design cheap. Supervision design, stim
design, redesign review - they were all this weekend, the last 5-10 hours of dev
time. Fan out is a thing for sure - there's the language, the text editor, the data
model, the emulator as you said - but they are *cheap* to build, and they compose
elegantly. I don't feel that I've hit my comprehension bandwidth with them either -
and in a way, exploring the frontier of this bandwidth is another goal of the
project. I'm challenging myself to keep the documentation discipline needed to
steward five or more different subprojects. The fan-out also has another benefit: I
can run multiple agents in parallel while still owning the commit step myself and
not having them toe-stepping.

"has an agent-off period actually happened?" Not in the literal sense. But I have
done complex Rust projects before with no agent (last year I built a DAW from
scratch with its own audio engine!); I have the learning track; I implemented
Stitch dispatch myself with agent guidance, which serves as a forced slowdown that
ran me through everything from maximal-munch to pratt precedence to vtables; and I
quiz myself regularly. Perhaps agent-off would help. Perhaps it would cost more
velocity than it gains in purity. Maybe it's worth a test to see which is which.
For what it's worth, I've written a toy sv39 walker in the learning dir, so "do I
hold this" is true in that case. IPC rendezvous I've been quizzed on and passed.

"Decorrelate your review." True. I don't think Gemini is going to tell me anything
I can't get myself or from Opus though; tasking a real engineer is expensive in
terms of their time and changes the nature of the project; maybe the takeaway is to
review more of the design myself (I've found weird seams doing this in the past).

"Consider inverting the goal weights in the story you tell." Maybe. What's
interesting to me though is that even though Toy OSes are plenty, there are none
that do capabilities, have a distributed-systems-tier observability story, live on typed
data, and combine these three facts into an elegant shell subsystem.
```
