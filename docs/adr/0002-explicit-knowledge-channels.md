# Clocks merge only through explicit knowledge channels

The coordinator holds every slot's clock, but it is not a clock participant and never sends its global merged view back to an agent. An agent's clock absorbs another agent's entries only through a knowledge channel: reading a file (merges that file's clock), being denied (merges the lease holder's clock from when it took the lease), or passing a barrier (merges every participant's clock). If the coordinator merged everything on every message, every agent would seem to know everything after one round trip, and `knew()` and the "what did it know" inspector would mean nothing.

## Consequences

The coordinator numbers events (`event_id`) for storage, but that order carries no causal meaning. Every new way information can pass between agents must be added as an explicit channel, or the trace will under-report knowledge.
