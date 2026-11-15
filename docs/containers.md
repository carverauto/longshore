# Running in containers

A container meets two limits that a plain Linux host does not. Both were
found by running the ring checks in a Kubernetes pod with a non-root user,
every capability dropped and the default seccomp profile, on containerd
2.3 and Linux 6.8.

The first is seccomp. The default profile of the container runtime refuses
the three `io_uring` system calls, so creating a ring fails with
"Operation not permitted". The pod needs a profile that is the runtime's
default plus `io_uring_setup`, `io_uring_enter` and `io_uring_register`.
`deploy/seccomp/containerd-2.3-x86_64-io-uring.json` is that profile for
containerd 2.3 on x86-64, for a container with no capabilities. It is the
profile the runtime generated for the test pod with one rule added, and the
ring checks pass under it, zero-copy sends included. A runtime's default
changes between versions and architectures, so for anything else start
from that runtime's own default and add the same rule.

In Kubernetes the file goes in the kubelet's seccomp directory on every
node that may run the pod, usually `/var/lib/kubelet/seccomp`, and the pod
names it:

```yaml
securityContext:
  seccompProfile:
    type: Localhost
    localhostProfile: containerd-2.3-x86_64-io-uring.json
```

Node configuration management or the Security Profiles Operator can place
the file. With Docker the same file is passed as
`--security-opt seccomp=<file>`. A node can also turn `io_uring` off for
everything with the `kernel.io_uring_disabled` setting, on Linux 6.6 and
later, and no profile changes that.

The second is locked memory. longshore registers its buffers with the
kernel, which locks them in memory, and that is what lets a body go out
without a copy. A process may lock only as much as its `RLIMIT_MEMLOCK`
allows. The test pod had a limit of 8 MiB that it could not raise, and
building the client failed with "Out of memory". That error now goes on to
say that `RLIMIT_MEMLOCK` is too low. The limit has to be raised
on the node. A chart cannot do it, and the `IPC_LOCK` capability is not
open to a pod under the `restricted` Pod Security level.

A container inherits the limit from the container runtime's service. On
the test node that was `k3s-agent`, with the systemd default of 8 MiB.
This drop-in and a restart of the service raised it:

```ini
# /etc/systemd/system/k3s-agent.service.d/10-memlock.conf
[Service]
LimitMEMLOCK=infinity
```

A pod started after that, with the same restricted settings and the
profile above, ran the ring checks and the client with registered buffers
and zero-copy sends. Pods that were already running kept the old limit.
The same setting goes on `containerd.service` where containerd runs as its
own service. It applies to every pod on the node, and every node that may
run the pod needs it. Managed Kubernetes services were not tested.

What is locked is each I/O thread's whole buffer pool: the buffers lent to
callers and the ones its connections use. That is most of the memory the
client allocates, so a limit that is not `infinity` has to be about the
size of the client. Locked memory is counted per user id on a node, so two
pods that run as the same user share one limit. The memory is allocated at
start-up, so the container's memory limit has to cover it too.
