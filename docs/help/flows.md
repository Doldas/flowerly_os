# Flows

A **Flow** is a persistent, evolvable operating context representing one coherent area of activity.

Examples include:

- General
- Development
- Server Operations
- Gaming
- Art
- Research

The design principle is:

> The user should switch contexts, not reconstruct them.

## General Flow

Every Flowerly OS installation is intended to have a neutral **General Flow** for ordinary and temporary work.

## What a Flow can eventually contain

A Flow may own or reference:

- projects and goals
- applications and windows
- persistent terminal sessions
- agents and memory context
- containers and remote sessions
- resource and notification policy
- security and credential scope
- its own evolving presentation

## Current implementation status

The data model and runtime foundations are being built before the graphical shell. Persistent PTY attach/detach/reattach behavior is part of the 0.0.4 Flows and Sessions milestone.

For architectural details, see [Flow Model](../flow-model.md) and [Roadmap](../roadmap.md).
