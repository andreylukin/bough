-module(bough_ffi).
-export([now_ms/0, hash/1]).

now_ms() -> erlang:system_time(millisecond).

%% Content hash for integrity tracking (engine guardrail): cheap, stable within
%% a run, used only to detect whether a pre-existing file changed.
hash(Data) -> erlang:phash2(Data).
