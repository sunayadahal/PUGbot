# PUGbot documentation set

## Scope of this document

This page indexes PUGbot's documentation and records the content model it
follows. It is the entry point for readers and the reference for anyone adding
a document.

## What the ISO standards actually govern

There is **no ISO standard for source-code comment or rustdoc syntax**. Comment
style in this repository follows Rust community convention, enforced
mechanically (see [Enforcement](#enforcement)).

The ISO/IEC/IEEE standards that *are* relevant govern documentation as a
product and as a process — which documents must exist, what content each
carries, and how they are managed:

| Standard | Subject | How it is applied here |
| --- | --- | --- |
| ISO/IEC/IEEE 15289 | Content of life-cycle information items | Defines which information items this set provides and the content each must carry. Mapped in the table below. |
| ISO/IEC/IEEE 26511 | Requirements for managers of information for users | Governs this plan: ownership, review, and the change rule below. |
| ISO/IEC/IEEE 26512 | Requirements for acquirers and suppliers of information for users | Informs the audience analysis that splits the player guide from the administrator guide. |
| ISO/IEC/IEEE 26513 | Requirements for testers and reviewers of information for users | Informs the verification approach: documented behaviour is asserted by tests where it can be. |
| ISO/IEC/IEEE 26514 | Design and development of information for users | Governs the structure of the user-facing guides: task orientation, consistent section pattern, worked examples. |
| ISO/IEC/IEEE 42010 | Architecture description | Governs the architecture document: stakeholders, concerns, and viewpoints. |
| ISO/IEC/IEEE 12207 | Software life-cycle processes | Context for which information items a project of this size needs at all. |

**This is an alignment claim, not a conformance claim.** No conformance
assessment has been performed, and several information items that a full 15289
application would require — a quality-assurance plan, a configuration-management
plan, an acquisition record — are deliberately absent because they do not apply
to a single-repository project with no external acquirer. The claim made here is
narrower and checkable: each document below carries the *content* that 15289
specifies for its information-item type.

## The document set

| Document | 15289 information item | Audience |
| --- | --- | --- |
| [`../README.md`](../README.md) | Software product overview; installation information | Everyone; first-time operators |
| [`architecture.md`](architecture.md) | System/software architecture description (42010 viewpoints) | Developers, reviewers |
| [`player-guide.md`](player-guide.md) | User documentation — operation | Players |
| [`administrator-guide.md`](administrator-guide.md) | User documentation — administration | Server administrators and moderators |
| [`operations.md`](operations.md) | Maintenance plan; operation and support information | Operators running a deployment |
| [`data-model.md`](data-model.md) | Database design description | Developers, operators |
| [`traceability.md`](traceability.md) | Requirements traceability record; verification results | Reviewers, maintainers |
| [`glossary.md`](glossary.md) | Terminology | Everyone |
| API reference (`cargo doc --open`) | Software interface description | Developers |

## Audience analysis

Three audiences, deliberately kept in separate documents rather than one manual
with conditional sections:

* **Players** want to join a game. They need a handful of commands and no
  concepts. The player guide never mentions ratings mathematics, schemas, or
  modes.
* **Administrators and moderators** configure a server. They need every setting
  explained with its consequence, and they need to know which actions are
  audited and which are irreversible.
* **Operators and developers** run or change the software. They need the
  architecture, the data model, the runbook, and the API reference.

## Document conventions

* British spelling, sentence case in headings.
* Every user-facing document opens with a **Scope** section stating who it is
  for and what it covers.
* Procedures are numbered steps with a stated outcome; reference material is
  tabular.
* Command names appear as `/command`, settings as `setting-name`, and code
  identifiers as `Identifier`.
* Every non-obvious statement about behaviour cites the test or source that
  makes it true, so a reader can check rather than trust.

## Enforcement

Documentation accuracy is enforced by the build wherever it can be, rather than
by review alone:

| Rule | Enforced by |
| --- | --- |
| Every public item is documented | `#![deny(missing_docs)]` in `src/lib.rs` |
| Every public type is inspectable | `#![deny(missing_debug_implementations)]` |
| Every doc link resolves | `#![deny(rustdoc::broken_intra_doc_links)]` |
| No doc links to private items | `#![deny(rustdoc::private_intra_doc_links)]` |
| Every fallible function documents its errors | `#![warn(clippy::missing_errors_doc)]`, promoted to an error by `-D warnings` in CI |
| Every panicking function documents its panics | `#![warn(clippy::missing_panics_doc)]`, likewise |
| Documented examples still work | rustdoc doctests, run by `cargo test` |
| Translations stay complete and well-formed | `localization::tests` — key parity and placeholder preservation |
| The command surface matches this documentation | `discord::commands::tests` — every specified command is registered |

CI runs `cargo fmt --all --check`, `cargo clippy --all-targets --all-features
-- -D warnings`, and `cargo test --all-features`, so a documentation regression
fails the build in the same way a code regression does.

## Change rule

A change to behaviour is not complete until the documents affected by it are
updated in the same commit. In practice:

| If you change… | Update… |
| --- | --- |
| A public item's signature or behaviour | Its rustdoc, including `# Errors` |
| A slash command or its options | [`player-guide.md`](player-guide.md) or [`administrator-guide.md`](administrator-guide.md), and the command table in [`../README.md`](../README.md) |
| A configuration setting | [`administrator-guide.md`](administrator-guide.md) and `.env.example` if it is an environment variable |
| The schema | A new migration, [`data-model.md`](data-model.md), and [`architecture.md`](architecture.md) if an invariant moved |
| A requirement's implementation status | [`traceability.md`](traceability.md) |
| An operational procedure | [`operations.md`](operations.md) |

## Maintenance

* **Owner:** the repository maintainers.
* **Review trigger:** any change matching the table above; otherwise at each
  release.
* **Source of truth:** the code. Where a document and the code disagree, the
  code is correct and the document is a defect — report it as one.
