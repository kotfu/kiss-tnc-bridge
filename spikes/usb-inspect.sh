#!/bin/sh
# What does a TH-D75 look like over USB? Run on the Pi with the radio plugged in.
echo "== lsusb =="; lsusb
echo; echo "== lsusb -t =="; lsusb -t
echo; echo "== /dev/serial/by-id =="; ls -l /dev/serial/by-id/ 2>&1
echo; echo "== /dev/serial/by-path =="; ls -l /dev/serial/by-path/ 2>&1
for tty in /dev/ttyACM* /dev/ttyUSB*; do
  [ -e "$tty" ] || continue
  echo; echo "== udevadm info $tty =="
  udevadm info -q property -n "$tty" | grep -E 'ID_VENDOR|ID_MODEL|ID_SERIAL|ID_USB_INTERFACE_NUM|ID_USB_INTERFACES|DEVLINKS|ID_MM|ID_PATH='
done
echo; echo "== kernel log (usb/cdc/tty) =="; dmesg 2>/dev/null | grep -i -E 'usb|cdc|tty' | tail -25 || journalctl -k --no-pager -n 200 | grep -i -E 'usb|cdc|tty' | tail -25
echo; echo "== ModemManager =="; systemctl is-active ModemManager 2>&1
for d in /sys/bus/usb/devices/*; do
  [ -f "$d/idVendor" ] || continue
  v=$(cat "$d/idVendor"); p=$(cat "$d/idProduct")
  case "$v" in 0b4d|2a1f|0579) echo; echo "== Kenwood-ish device $v:$p at $d =="; cat "$d/manufacturer" "$d/product" "$d/serial" 2>/dev/null; echo "bNumInterfaces=$(cat $d/bNumInterfaces) bDeviceClass=$(cat $d/bDeviceClass)"; for i in "$d":*; do echo "  iface $(basename $i): class=$(cat $i/bInterfaceClass 2>/dev/null) sub=$(cat $i/bInterfaceSubClass 2>/dev/null) proto=$(cat $i/bInterfaceProtocol 2>/dev/null) $(ls $i 2>/dev/null | grep -E '^tty' | tr '\n' ' ')"; done;; esac
done
