# kisd

kisd is a daemon which implements parts of the KIS / DebugUSB protocol.
The main use at this time is accessing the "dockchannel uart" of Apple
Silicon machines from a device running Asahi Linux.


### Usage

- Start kisd
  - kisd will allocate a pseudo-terminal and create a symlink at /dev/m1n1
  - kisd will continuously scan for DebugUSB devices and attempt to attach the dockchannel uart to /dev/m1n1
- Connect your target device and put it into debugusb mode: `tuxvdmtool [reboot] debugusb` (TODO: integrate tuxvdmtool into kisd?)
- Run your m1n1 proxyclient commands against /dev/m1n1 as usual, or connect with picocom (baud rate doesn't matter)


### Setup
For use as non-root user a udev rules file is provided to change the group
of the USB device to 'dialout'. In this case the '/dev/m1n1' symlink will
not be created and the pty has to be used directly, for example via
`export M1N1DEVICE=/dev/pts/X`.
This requires following install steps:
- install etc/udev/rules.d/85-apple-debugusb.rules to /etc/udev/rules.d/
- `sudo udevadm control -R`


### Base addresses

Currently it is not known how the correct write addresses to use in the DebugUSB messages for input / key presses are determined based on the previous handshake messages. If no `--base` is specified, kisd will attempt to guess the address in a way which works for some devices, but not all.

The following working base addresses have been determined based on Wireshark USB dumps of the DebugUSB communication under macOS.

| Chip    | Codename | Protocol Version (bcdDevice) | Base        | auto-detected      | verified           |
| ------- | -------- | ---------------------------- | ----------- | ------------------ | ------------------ |
| M1      | t8103    |                         1.20 | 0x23d000000 | :white_check_mark: | :white_check_mark: |
| M1 Pro  | t6000    |                         1.20 | 0x292400000 | :x:                | :white_check_mark: |
| M1 Max  | t6001    |                         1.20 | 0x292400000 | :x:                | :white_check_mark: |
| M1 Ultra| t6002    |                         1.20 | 0x292400000 | :x:                | :white_check_mark: |
| M2      | t8112    |                         2.00 | 0x23d000000 | :white_check_mark: | :white_check_mark: |
| M2 Pro  | t6020    |                         3.00 | 0x29e400000 | :x:                | :white_check_mark: |
| M2 Ultra| t6022    |                         3.00 | 0x29e400000 | :x:                | :white_check_mark: |
| M3      | t8122    |                         4.00 | 0x2e4000000 | :white_check_mark: | :white_check_mark: |
| M3 Max  | t6031    |                         4.00 | 0x2a0400000 | :x:                | :white_check_mark: |
| M4      | t8132    |                         4.00 | 0x3c8000000 | :white_check_mark: | :white_check_mark: |
| M4 Pro  | t6040    |                         4.00 | 0x548700000 | :x:                | :white_check_mark: |
| A18 Pro | t8140    |                         4.00 | 0x348000000 | :white_check_mark: | :white_check_mark: |

### Credits

Thanks to Sven Peter for the earlier work and documentation on DebugUSB and Fiona Behrens for help with USB basics.
