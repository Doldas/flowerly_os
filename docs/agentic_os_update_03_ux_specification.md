# Agentic OS — Update 03: Human-Centric Experience Architecture & Implementation Roadmap

> **"The operating system should adapt to human intent, non-verbal context, and cognitive energy rather than forcing humans to master application windows and system settings."**

**Document Type:** UX Architecture & Feature Implementation Specification  
**Applies to:** `AGENTIC_OS_MASTER_VISION.md`, `AGENTIC_OS_UPDATE_01_AMBIENT_INTERACTION.md`, `AGENTIC_OS_UPDATE_02_PERSISTENT_FLOWS.md`  
**Focus:** Adaptive visual environments, multimodal intent parsing, cognitive workload reduction, context-aware privacy, and human-in-the-loop evolution.

---

## 1. Vision Extension: Cognitive-State Aware Computing

Traditional operating systems treat all human interactions uniformly regardless of whether the user is a focused software engineer, a 6-year-old artist exploring creativity, or an exhausted professional managing an incident. 

Agentic OS Update 03 introduces **Cognitive-State Awareness**. The system observes interaction velocity, input modalities (pen, voice, touch, keyboard), error rates, and Flow usage patterns to infer cognitive load and adjust UI density, notification aggressiveness, and AI intervention styles.

```text
                               HUMAN INTENT & STATE
                       (Voice, Pen, Touch, Keys, Velocity)
                                        │
                                        ▼
                         Ambient Multimodal Processor
                                (`ambientd`)
                                        │
                                        ▼
                           Intent & Context Engine
                                 (`intentd`)
                                        │
                    ┌───────────────────┼───────────────────┐
                    ▼                   ▼                   ▼
           Visual Metaphor         Adaptive Tool        Substrate
              Generator              Assembly          Optimization
            (`metathemeg`)          (`dockd`)           (`flowd`)
                    │                   │                   │
                    └───────────────────┼───────────────────┘
                                        ▼
                              Generated UI Canvas
                         (Wayland / Reactive Render)
```

---

## 2. Core User Experience Architecture

### 2.1 The Generative Visual Shell (`metathemeg`)

Instead of fixed desktop themes (e.g., GTK/Qt themes), Agentic OS introduces **Dynamic Visual Metaphors** generated and refined per Flow.

#### For the 6-Year-Old "Horse & Creative" Environment:
* **Organic Canvas Navigation:** Applications and directories are replaced by spatial "paddocks" and "stables" (e.g., *My Drawings*, *Story World*, *Yesterday's Horse*).
* **Tactile Affordances:** Buttons use soft, organic geometry, exaggerated press depths, and gentle auditory feedback (pastel nature tones).
* **Progressive UI Exposure:** When the child draws a horse and asks *"How do I make it jump?"*, the OS doesn't open a video editor. It overlays a lightweight animation timeline with visual handles directly on the canvas.

#### For the Engineering / System Admin Environment:
* **Information-Dense Workbench:** Screen real estate prioritizes source trees, persistent terminal outputs, live eBPF telemetry, and active agent execution graphs.
* **Keyboard-First Omnibar:** Radial and visual elements collapse into an ultra-low-latency command palette and hotkey-driven Flow switches.

---

### 2.2 Intent-Driven Multimodal Assembly (`dockd`)

When a user expresses a multi-step desire, `intentd` resolves the necessary tools and `dockd` dynamically composite a single unified workspace, bypassing manual application launcher workflows.

```text
User: "I want to design a storybook about wild horses and record a voiceover."

                ┌───────────────────────────────────────────────┐
                │ Intent Resolver (`intentd`)                   │
                └──────────────────────┬────────────────────────┘
                                       │
                                       ▼
    ┌───────────────────────────────────────────────────────────────────────┐
    │ Dynamic Spatial Workspace (`dockd`)                                   │
    │                                                                       │
    │  ┌─────────────────────────┐  ┌────────────────────────────────────┐  │
    │  │ Canvas / Drawing Area   │  │ Reference Board                    │  │
    │  │ (Pen / Touch priority)  │  │ (Equine poses & lighting refs)     │  │
    │  └─────────────────────────┘  └────────────────────────────────────┘  │
    │  ┌─────────────────────────┐  ┌────────────────────────────────────┐  │
    │  │ Local Audio Recorder    │  │ Creative Narrative Assistant       │  │
    │  │ (Low-latency PipeWire)  │  │ (Story structure & prompts)        │  │
    │  └─────────────────────────┘  └────────────────────────────────────┘  │
    └───────────────────────────────────────────────────────────────────────┘
```

---

### 2.3 Physical Privacy & Local-First Ambient Pipeline

To guarantee trust without sacrificing convenience, ambient listening strictly operates on an isolated, local pipeline.

```text
   [ Microphones ]
          │
          ▼
   ┌──────────────┐    [ Physical HW Switch OFF ] ──► System Power Cut
   │  PipeWire    │                                   (Software Cannot Override)
   └──────┬───────┘
          │
          ▼
   ┌──────────────┐
   │  VAD / Wake  │ ──► Local Silero VAD / On-Device Wake-word engine
   └──────┬───────┘
          │
          ▼
   ┌──────────────┐
   │ Speech-to-Text│ ──► Local Whisper C++ Model (0 Cloud Transmission)
   └──────┬───────┘
          │
          ▼
   ┌──────────────┐
   │ Attention    │ ──► Evaluates: Spatial focus, active gaze, hotkey state,
   │  Filter      │     and conversational context
   └──────────────┘
```

---

## 3. Detailed Feature Specifications

| Feature Name | Primary User Persona | System Implementation | Core Benefit |
| :--- | :--- | :--- | :--- |
| **Scratchpad Flows** | Developers, Researchers, Children | Ephemeral `flowd` instances running inside isolated cgroup scopes. Automatically checkpointed and garbage-collected after inactivity. | Allows quick experimentation without cluttering main work environments. |
| **Spatial Intent Pointing** | Artists, Touch/Pen Users | `intentd` resolves visual screen coordinates during voice input (e.g., holding pen on canvas while saying *"Fix this color"*). | Eliminates explicit text commands for spatial tasks. |
| **Flow-Aware Hardware Control** | Gamers, Musicians, Creators | Daemon (`policy-bridged`) dynamically shifts PipeWire buffer sizes, GPU power profiles, and input polling rates based on active Flow. | Replaces manual hardware tweaking with automatic, context-driven optimization. |
| **Progressive Abstraction Ladder** | Growing Learners, Non-Technical Users | `metathemeg` tracks mastery indicators and offers optional UI expansions (Visual Blocks → Scripting → Terminal). | Allows the operating system to grow with the user over years. |
| **Explainable UX Rollback** | All Users | `provenanced` tracks UI layout modifications. Saying *"Put it back"* immediately reverts layout and marks the adaptation hypothesis as rejected. | Ensures the OS never locks the user into unwanted or frustrating UI changes. |

---

## 4. Implementation Roadmap (Sprints 11–16)

Following the initial kernel substrate and core daemon bootstrap (Sprints 0–10), the UX roadmap focuses on turning the substrate into an adaptive, ambient experience.

```text
Sprint 11 ────────► Sprint 12 ────────► Sprint 13 ────────► Sprint 14 ────────► Sprint 15 ────────► Sprint 16
Local Ambient       Flow Engine         Dynamic Workspace   User Model          Generative UI       End-to-End
Pipeline            Integration         Assembly            Evolution Engine    Metaphors           Personalization
(`ambientd`)        (`flowd` GUI)       (`dockd`)           (`memoryd`)         (`metathemeg`)      Release
```

---

### Sprint 11 — Local Ambient Interaction Pipeline
**Target Release:** `0.2.0`

#### Deliverables
* Implement `ambientd` daemon in Rust for low-overhead audio buffer management via PipeWire.
* Integrate local Voice Activity Detection (VAD) and Whisper.cpp bindings for on-device speech processing.
* Create `attentiond` filtering logic to distinguish background ambient chatter from direct system intent.
* Build a system-wide hardware microphone state indicator protocol connected to physical/sysfs status.

#### Acceptance Criteria
* Voice commands parsed locally under 350ms latency on modern x86_64 CPU without network calls.
* Hardware microphone disable switch physically prevents `ambientd` from receiving audio streams.

---

### Sprint 12 — Flow Engine GUI & Persistent Session Reconnection
**Target Release:** `0.3.0`

#### Deliverables
* Build a lightweight Wayland-native Flow Switcher overlay (triggered via hotkey, gesture, or voice command).
* Implement visual state indicators for Flows: `FOREGROUND`, `ACTIVE`, `QUIESCENT`, `HIBERNATED`, and `ARCHIVED`.
* Connect Wayland terminal viewports directly to persistent `sessiond` PTYs.

#### Acceptance Criteria
* Closing a Wayland window running an active process (e.g., compilation or rendering job) leaves the process executing inside `sessiond`.
* Re-opening or switching back to the Flow instantly re-attaches the presentation viewport to the running session.

---

### Sprint 13 — Dynamic Workspace Assembly (`dockd`)
**Target Release:** `0.4.0`

#### Deliverables
* Create `dockd` workspace compositor service to dock, tile, and group applications based on `intentd` output.
* Build pre-configured workflow templates for Common Intent archetypes (*Creative Drawing*, *Software Debugging*, *Incident Response*).
* Implement "Scratchpad Flow" sandboxing with automatic resource cleanup after 24 hours of inactivity.

#### Acceptance Criteria
* User saying *"I want to draw"* automatically provisions a focused canvas, asset reference panel, and pen-optimized controls while muting background notification toasts.

---

### Sprint 14 — User Mind Model & Inference Ledger
**Target Release:** `0.5.0`

#### Deliverables
* Implement SQLite-backed `user_model` database in `memoryd` separating `EXPLICIT`, `OBSERVED`, and `INFERRED` facts.
* Build confidence decay algorithms for inferred preferences (e.g., if a user rejects a layout change, inference confidence drops to 0).
* Create an administrative CLI tool (`agenticctl user-model list`) and GUI panel for inspecting and clearing learned preferences.

#### Acceptance Criteria
* Inferred preferences are never written directly to system state without passing confidence thresholds or explicit user confirmation.
* User can view, edit, or delete any inferred fact about their preferences at any time.

---

### Sprint 15 — Generative UI Metaphors & Progressive Abstraction
**Target Release:** `0.6.0`

#### Deliverables
* Implement `metathemeg` renderer capable of morphing UI density, iconography, and spatial presentation.
* Build the "Horse & Creative" spatial visual workspace generator for young/artistic profiles.
* Implement Progressive Abstraction handles (enabling non-technical views to reveal underlying logic blocks or code on demand).

#### Acceptance Criteria
* The same core OS installation successfully renders as a soft, spatial creative workspace for a child profile and as a dense, metrics-driven workbench for an engineering profile.

---

### Sprint 16 — End-to-End Human Personalization Release
**Target Release:** `0.7.0`

#### Deliverables
* Full integration of Ambient Interaction, Flow Engine, User Model, and Evolutionary UI Generation.
* Comprehensive automated end-to-end testing suite for UI rollback, generation bisecting, and ambient command execution.
* Packaging of installation images (`.iso`) with pre-installed base seed environment.

#### Acceptance Criteria
* Complete execution of the **"Unicorn/Horse Test"**: A clean seed installation successfully transforms its visual theme, tool assembly, and interaction style into a tailored creative environment through natural usage without losing its full Linux capabilities or requiring a system reboot.

---

## 5. Architectural Safeguards & Non-Negotiables

1. **AI Failure Non-Blocker:** If LLM inference daemons crash or network connections fail, the visual environment immediately falls back to a deterministic, reliable GTK/Wayland shell.
2. **Strict Provenance Tracking:** Every dynamic change to window layouts, themes, or application groupings must log an explicit entry into `provenanced` detailing *why* the change occurred.
3. **No Unwanted Behavioral Manipulation:** Adaptation algorithms must explicitly optimize for goal completion, reduced cognitive friction, and creative expression—never for engagement time or screen addiction.