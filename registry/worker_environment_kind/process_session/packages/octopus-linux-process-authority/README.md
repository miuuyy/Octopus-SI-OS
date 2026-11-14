# Linux process-authority service

This package supplies the privileged AppArmor profile loader used by the
platform process-authority boundary. The runtime, verifiers, interface lives
and tools continue to run as the operator's Unix user.

The service loads profiles from structured path and operation declarations,
attaches the preparing process or its children to their profile's cgroup, and
releases profiles after the preparation owner and every descendant have gone.
It accepts no command to execute, no caller-selected PID, no raw AppArmor text
and no change to another user's profile. A restart retains confinement and
resumes collection from the service's own records.

The installed executable also supplies an unprivileged `exec` entry point for
formed workspace commands. That process requests a profile through the same
service and replaces itself with the command, preserving its PID and streams.
Only the service invocation runs as root.

See [Linux process isolation](../../../../../docs/linux.md) for requirements,
installation, lifetime semantics and diagnostics. The installer is run by an
administrator; Octopus never installs or starts this service implicitly.
