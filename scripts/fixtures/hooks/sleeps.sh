#!/bin/sh
# Never answers in time. The timeout fixture.
cat > /dev/null
sleep 30
printf '{}\n'
