#!/bin/sh
# Every benchmark binary, with shorter warm-up and measurement than
# criterion's 3 s + 5 s: stable hardware needs fewer samples, and the free
# plan's jobs stop at 5 minutes.
set -e
for b in /bench/bin-*; do
    "$b" --bench --warm-up-time 1 --measurement-time 3
done
