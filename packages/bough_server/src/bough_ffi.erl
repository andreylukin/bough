-module(bough_ffi).
-export([now_ms/0, hash/1, with_session_lock/2]).

now_ms() -> erlang:system_time(millisecond).

%% Content hash for integrity tracking (engine guardrail): cheap, stable within
%% a run, used only to detect whether a pre-existing file changed.
hash(Data) -> erlang:phash2(Data).

%% Node-wide lock keyed by session id, so concurrent runs on different branches
%% serialize their read-modify-write of the one session file (no lost updates).
%% global:trans blocks until the lock is acquired and releases it automatically
%% when Fun returns *or* the holding process dies — so there are no stale locks.
with_session_lock(Key, Fun) ->
    global:trans({{bough_session, Key}, self()}, Fun).
