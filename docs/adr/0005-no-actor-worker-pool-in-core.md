# No Actor or WorkerPool in Core

**Status:** accepted

Actor placement, leases, fencing, autoscaling and Kubernetes are large
distributed-systems concerns. Building them into Core would make the framework
heavier without serving the default local use case.

We decided these belong to a future, separate `agent-compute` component. The
framework keeps only the design principles that logical work does not require a
permanent process and capacity is bounded.

Consequences: local use relies on Tokio semaphores and bounded queues. There is
no automatic multi-machine placement or distributed failover.

Revisit when single-machine and hosted deployments demonstrate a real capacity
or placement need.
