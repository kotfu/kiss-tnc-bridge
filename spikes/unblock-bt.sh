#!/bin/sh
# Clear rfkill soft blocks on all Bluetooth adapters (needs root).
for f in /sys/class/rfkill/rfkill*; do
  if [ "$(cat $f/type)" = "bluetooth" ]; then
    echo 0 > $f/soft
    echo "$(cat $f/name): soft=$(cat $f/soft) hard=$(cat $f/hard)"
  fi
done
