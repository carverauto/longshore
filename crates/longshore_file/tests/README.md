# The tests of `longshore_file`, against the contract's tables

The contract is in `openspec/changes/add-file-crate/design.md` ("The
contract"). Each row of its tables C3, C4, C5, C10 and C11 is listed below
with the tests that reach it.

## Where the tests are

| Name here | What | Runs on |
|---|---|---|
| `check::…` | the contract suite, `src/check/` (`check::run_all`) | `SimIo` and `PosixIo` (`file_contract`: `sim`, `posix`), `UringIo` and `PosixIo` on Linux (`longshore-bench` binary `files_uring`) |
| `contract::…` | what only the simulator can be made to do, `tests/contract/` (`faults`, `resumes`, `power`) | `SimIo` |
| `model` | the model test, `tests/model/`: random runs, F1 to F10 after every step, six campaigns | `SimIo` with its disk |
| `alloc` | the allocation guard, `tests/alloc/` | `SimIo` (with power loss), `PosixIo` |
| `props` | property tests (Hegel), `tests/props/` | the plan alone, `SimIo` |
| `units` | the implementation's own unit tests, `tests/units/` | `SimIo` |

The model test checks the invariants after every step and every turn:
F1 and F6 from what the set submits (seen through `check::Spy`), F2 and
F5 from the events, F3 from the extents' holds, F4 and F10 from
`usage()` and the tickets in flight, F7 from every read against the
model's bytes, F8 after every power loss by reading back every byte the
model was promised, F9 by sealing every call of the set and the backend.

## C3, a file slot

| State | Entered by | Left by | Reached by |
|---|---|---|---|
| Free | build | `open` accepted | every test that opens a file |
| Free | a close's completion | | `check::closing::generations`, `check::refusals::stale_handles`, `model` |
| Free | a failed open | | `check::closing::generations` (generation + 1), `check::basics::open_with_each_flag`, `model` |
| Free | `power_lost` | | `contract::power::power_loss_keeps_what_was_synced_and_ends_what_was_in_flight`, `alloc`, `model` (PowerLoss, Torn, Silent) |
| Opening | `open` accepted; accepts nothing | the open's completion: success → Open, failure → Free | `check::basics::events_name_what_they_are_of`, `check::closing::generations`, `model` (an open's slot counted in `usage().files()`) |
| Open | the open's success; every operation | `close` → Closing | every suite |
| Open | | a failed sync → Poisoned | `contract::faults::a_failed_sync_poisons_the_file_and_reads_and_its_close_go_on` (Fail and FailAfter), `model` (SyncFails) |
| Open | | a failed write on a durable-write file → Poisoned | `contract::faults::a_failed_durable_write_poisons_and_a_failed_buffered_one_does_not`, `model` (Reported, SyncFails) |
| Poisoned | accepts `read_at` and `close`, the rest `Poisoned` | `close` → Closing | `contract::faults::a_failed_sync_poisons_…` (write, spans, sync, reserve, punch and a step refused; read and close taken), `model` (F6 on what is submitted) |
| Closing | `close` accepted; accepts nothing (`Closing`) | the close's completion → Free, generation + 1 | `check::closing::close_is_held_back`, `model` |
| Closing | close held back until every other operation has reported | | `check::closing::close_is_held_back` (nothing submitted at the close, the close's event last), `check::bounds::one_file_at_its_cap`, `alloc`, `model` (closes with operations in flight) |
| Closing | close never `Full` | | `check::bounds::one_file_at_its_cap` (the close of a file at its cap) |
| Closing | a close error is reported and the slot freed | | `contract::power::a_failed_close_frees_its_slot` |
| (all) | generations are `u32` and wrap | | **not reached**: four billion opens of one slot |

## C4, an operation slot

| State | Entered by | Left by | Reached by |
|---|---|---|---|
| Free | build; its event taken; `power_lost` | accepted | every suite; `power_lost`: `contract::power::…`, `alloc`, `model` |
| Waiting | accepted without room at the backend (`Busy`) | room: → Submitted, first in first out | `check::bounds::waiting_goes_first_in_first_out` (on every backend, room taken by `check::Spy`), `contract::faults::waiting_work_of_a_poisoned_file_ends_poisoned_unsubmitted`, `alloc`, `model` (squeezed room) |
| Waiting | a sequence step behind an earlier one | | `check::bounds::waiting_goes_first_in_first_out` (a waiting sequence is not overtaken by a single operation after it), `model` |
| Submitted | given to the backend | whole → Reported | every suite |
| Submitted | | short → Resuming | `contract::resumes::a_short_write_is_resumed_for_the_rest`, `…a_short_direct_write_resumes_from_its_last_whole_block`, `props` (`a_write_cut_short_any_number_of_times_ends_whole`), `model` (Reported, SyncFails, Torn) |
| Submitted | | failed → Reported | `contract::faults::the_disks_errors_map_to_one_type`, `model` |
| Resuming | a short transfer | the rest → Submitted | as above |
| Resuming | a broken chain | every member reported → the rest Submitted | `contract::resumes::a_chain_broken_by_a_short_write_goes_on_in_order` (two chains seen), `check::sequences::a_short_read_is_not_a_failure` (a read at the end of the file breaks the backend's chain on every backend) |
| Reported | its outcome decided | `next_event` returns it → Free | every suite; F2 and F10 in `model` |
| (queue) | the event queue never overflows | | `model` (every run's events, up to every slot at once) |

## C5, operations

| Operation | Needs | Holds | Result | Reached by |
|---|---|---|---|---|
| `open(path, flags)` | a free file slot (`Full` without) | its path buffer | 0, the `File` in the event | `check::basics::open_with_each_flag` (read only, write only, create, exclusive → `Exists`, empty, durable writes, directory, missing → `NotFound`), `check::direct::direct_io` (direct), `check::refusals::extents_that_cannot_be_held` (`Full`), `check::basics::events_name_what_they_are_of` (the event names the new file), `model` |
| `close(file)` | – | – | 0 | every suite; `check::closing::*` |
| `read_at` | not write-only (`NotOpen`) | the extent | bytes read; fewer only at end of file | `check::basics::write_sync_read_back` (across and past the end), `check::transfers::*`, `check::basics::open_with_each_flag` (`NotOpen`), `model` (F7 on every read) |
| `write_at` | opened for writing (`NotOpen`) | the extent | all, or an error | `check::transfers::*` (held while in flight, let go of at the event), `check::basics::open_with_each_flag` (`NotOpen`), `model` |
| `write_spans` | opened for writing | each extent span; the slot's span list | as `write_at` | `check::transfers::gathered_writes` (spans of three extents, in order), `contract::power::a_page_of_the_receive_area_is_a_span` (`Span::Page`, held by the caller only), `alloc`, `model` (extent and page spans) |
| `sync(file, data_only)` | – | – | 0 | `check::basics::write_sync_read_back` (both), `check::basics::open_with_each_flag` (of a read-only file, of a directory), `model` (F8 promotion) |
| `reserve(off, len, keep)` | writing, `len > 0` (`TooLarge`) | – | 0 | `check::basics::space` (grows, or keeps the size), `check::refusals::past_the_plan_and_the_extent` (len 0), `model` |
| `punch(off, len)` | writing, `len > 0`, whole blocks (`Unaligned`) | – | 0 | `check::basics::space` (zeros read back, size kept; macOS may refuse `Unsupported`), `check::direct::punch_is_whole_blocks`, `props` (`direct_io_is_refused_exactly_off_a_block`), `model` |
| `status(path)` | a path slot | path buffer, status area | the size | `check::basics::paths`, `check::bounds::one_file_at_its_cap` (a second path operation `Full`), `model` (size against the model) |
| `rename`, `unlink`, `mkdir` | a path slot | path buffer | 0 | `check::basics::paths` (and `NotFound`, `Exists`), `check::sequences::steps_of_several_files_and_a_path` (a rename as a step), `model` (names, and their durability after power loss) |
| (paths) | longer than `path_max` or a NUL → `BadPath` | | | `check::refusals::bad_paths` (every operation that names a path, and a step; a path of `path_max` bytes taken) |
| (directories) | a directory is a file, synced to make a create or rename durable | | | `check::basics::paths`, `contract::power::power_loss_keeps_what_was_synced_…`, `model` (F8: a name is promised once its directories are synced) |

## C10, errors and poisoning

| Error | Reached by |
|---|---|
| `Full` | `check::bounds::*`, `check::refusals::extents_that_cannot_be_held`, `props` (`no_file_takes_every_slot`), `model` (Full exactly where C2 allows it) |
| `Stale` | `check::refusals::stale_handles` (every operation and a step), `check::closing::*`, `contract::power::*`, `model` |
| `Closing` | `check::closing::close_is_held_back`, `model` |
| `Poisoned` (refusal) | `contract::faults::a_failed_sync_poisons_…`, `…a_failed_durable_write_poisons_…`, `model` |
| `Poisoned` (waiting work ends) | `contract::faults::waiting_work_of_a_poisoned_file_ends_poisoned_unsubmitted`, `model` |
| `Partial { kind: Poisoned }` (transfer in progress) | `contract::faults::a_transfer_in_progress_ends_partial_poisoned` |
| `NotOpen` | `check::basics::open_with_each_flag`, `model` |
| `Unaligned` | `check::direct::*`, `props`, `model` |
| `TooLarge` | `check::refusals::past_the_plan_and_the_extent`, `alloc`, `model` |
| `BadPath` | `check::refusals::bad_paths`, `alloc` |
| `StaleExtent` | `check::refusals::extents_that_cannot_be_held`, `model` |
| `TooManyHolds` | `check::refusals::extents_that_cannot_be_held` |
| `Cancelled` | `check::sequences::a_failure_ends_the_rest` (and the cancelled write not performed), `model` (F5) |
| `NoProgress` | `contract::resumes::an_error_after_some_bytes_is_partial_and_none_is_no_progress` (a direct write cut inside its first block; the simulator never cuts a buffered write to nothing), `props` |
| `Partial { done, kind }` | `contract::resumes::an_error_after_some_bytes_…` (`Io` and `NoProgress` after some bytes), `model` |
| `NotFound`, `Exists` | `check::basics::*`, `model` |
| `NoSpace`, `Unsupported` | `contract::faults::the_disks_errors_map_to_one_type` (refused reserve and punch), `model` (Reported) |
| `SyncFailed { errno }` | `contract::faults::a_failed_sync_poisons_…`, `model` (SyncFails) |
| `Io { errno }` | `contract::faults::the_disks_errors_map_to_one_type` (read, buffered write, read error), `model` |
| `Denied` | **not reached**: no backend in the tests refuses an open for want of permission (the Linux checks run as root) |
| `Invalid { errno }` | **not reached**: impossible after the set's checks; nothing makes a backend refuse what they let through |
| `Os { errno }` | `check::refusals::bad_paths` reaches a backend with a name too long, but only asserts the path was taken |
| a failed sync poisons whatever its errno, except `Unsupported` | the poisoning: as above; the exception **not reached**: no backend in the tests fails a sync `EOPNOTSUPP` |
| a failed sync: the bytes since the last good sync are not made durable by a later one | `model` (F8 in SyncFails: no promise after a failed sync of the file) |
| a failed buffered write does not poison | `contract::faults::a_failed_durable_write_poisons_and_a_failed_buffered_one_does_not`, `contract::faults::the_disks_errors_map_to_one_type` |
| reads stay allowed on a poisoned file | `contract::faults::a_failed_sync_poisons_…` |

## C11, refusals at the call

Every refusal below is checked by `check::Rig::refused` (in the suite)
or `model`'s `Run::judge`, which both hold that a refusal submits
nothing, takes no slot and no hold, and reports nothing.

| Refusal | Reached by |
|---|---|
| `Full` | `check::bounds::one_file_at_its_cap`, `check::bounds::a_sequence_needs_room_for_every_step`, `check::refusals::extents_that_cannot_be_held`, `props`, `model` |
| `Stale` | `check::refusals::stale_handles`, `check::closing::generations`, `model` |
| `Closing` | `check::closing::close_is_held_back`, `model` |
| `Poisoned` | `contract::faults::a_failed_sync_poisons_…`, `model` |
| `NotOpen` | `check::basics::open_with_each_flag`, `model` |
| `Unaligned` | `check::direct::*`, `props::transfers::direct_io_is_refused_exactly_off_a_block`, `model` |
| `TooLarge` | `check::refusals::past_the_plan_and_the_extent`, `alloc`, `model` |
| `BadPath` | `check::refusals::bad_paths`, `alloc` |
| `StaleExtent` | `check::refusals::extents_that_cannot_be_held`, `model` |
| `TooManyHolds` | `check::refusals::extents_that_cannot_be_held` |

## Replaying the model test

A failing run names its campaign and seed:

```sh
FILE_MODEL=SyncFails:113 cargo test -p longshore_file --test file_model replay -- --nocapture
FILE_MODEL_SEEDS=20000 cargo test --release -p longshore_file --test file_model
```
