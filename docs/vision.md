# Vision

Flowerly OS is a Linux distribution whose defining property is that the
operating environment itself can evolve. It is not Linux with a chatbot: agents
are part of the system's architecture, able to observe the machine, understand
goals, operate Linux, develop software, test candidate changes, and evolve the
userspace over time.

## The experience should be simple

The sophistication belongs underneath the experience. An ordinary user should be
able to say:

- "Continue the thing I was drawing."
- "Go to my art flow."
- "Back to work."
- "Show the server dashboard."

and the system switches context, restores the relevant sessions, projects, and
applications, and continues where they left off, without exposing the machinery
that makes it possible.

## Intent before operations

The user should not have to translate a human goal into application, file, menu,
package or command operations. Flowerly OS performs that translation.

```text
human expression
      ↓
    Intent
      ↓
context resolution
   ↙       ↘
action     persistent Goal
```

Intent is distinct from Goal. "Show the servers" may only switch context;
"reduce boot time by 20%" may create a durable Goal with evidence and success
criteria. See [RFC 0001](rfcs/0001-intent-as-first-class-domain.md).

## The system should understand itself

Behind that experience, Flowerly OS should be able to:

- observe its own behavior and gather evidence;
- form hypotheses and create persistent improvement goals;
- inspect and modify its own source through the same pipeline humans use;
- build candidate generations from source;
- boot and test candidates in a sandboxed evolution environment;
- benchmark and compare candidates against a baseline;
- reject poor candidates and promote good ones;
- record provenance linking every change to its goal, evidence, and generation;
- remove what no longer serves a purpose.

## Seed OS

The first installation is a seed OS. Two people may install the same image and,
years later, run systems that differ radically because each machine adapted to
different hardware, workflows, preferences, accessibility needs, and interests.
The system should grow with its owner instead of forcing the owner to abandon
one environment and learn another.

## Adaptation is not engagement optimization

The objective is usefulness: accomplishing goals, reducing friction, improving
reliability and accessibility, and preserving user control. It is explicitly
not the maximization of screen time, clicks, notifications, or dependency.
Personalization, especially for children, must never become manipulation.

## Distinguishing fact from inference

The system keeps three categories of user information separate:

- **Explicit** — the user stated it.
- **Observed** — the system watched behavior.
- **Inferred** — the system formed a hypothesis, with confidence.

An inference must never silently become an explicit fact, and every inference
should carry provenance, confidence, and revision state.

## Scope of evolution

Not every improvement belongs in the same place:

- **Distribution evolution** — general improvements for Flowerly OS itself.
- **Machine evolution** — adaptation to one computer.
- **User evolution** — adaptation to one person.

This separation keeps personal preference from becoming global distribution
behavior, while still allowing a personal discovery to be generalized through
evidence.

## Root of trust

The stable system a user relies on is separate from the Evolution World where
aggressive experiments happen. Reversibility is a first-class property, Linux
remains upstream, and the model provider is a replaceable component. The
operating system must remain a usable Linux system even when no AI provider is
reachable.

## Long-term objective

Flowerly OS must be capable of becoming something its original developers did
not design. The architecture exists to make that possible safely: every
autonomous change observable, attributable, and reversible.
