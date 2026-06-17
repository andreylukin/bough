-module(bough_ffi).
-export([now_ms/0]).

now_ms() -> erlang:system_time(millisecond).
