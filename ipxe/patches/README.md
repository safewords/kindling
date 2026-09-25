# Patches to upstream iPXE

Any `*.patch` here is applied, in file-name order, with `git apply` to the
pinned checkout before it is built. There are none yet, and the aim is to keep
it that way: everything this project needs so far is reachable through
`config/local/` and the embedded script, which survive an upstream bump
without being rebased.

When one is unavoidable, name it `NNNN-what-it-does.patch`, say in its header
why configuration could not do the same thing, and expect to rebase it every
time `UPSTREAM` moves. The build keys its checkout on the patch contents, so
adding, removing or editing one gives a clean tree rather than patching an
already-patched one.
