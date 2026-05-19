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


### Base addresses

Currently it is not known how the correct write addresses to use in the DebugUSB messages for input / key presses are determined based on the previous handshake messages. If no `--base` is specified, kisd will attempt to guess the address in a way which works for some devices, but not all.

The following working base addresses have been determined based on Wireshark USB dumps of the DebugUSB communication under macOS.

| Chip    | Codename | Protocol Version (bcdDevice) | Base        | Guessed |
| ------- | -------- | ---------------------------- | ----------- |-------- |
| M1      | t8103    |                         1.20 | 0x23d000000 | ?       |
| M1 Pro  | t6000    |                         1.20 | 0x292400000 | ?       |
| M2      | t8112    |                         2.00 | 0x23d000000 | ?       |
| M2 Pro  | t6020    |                         3.00 | 0x29e400000 | ()      |
| M3      | t8122    |                         4.00 | 0x2e4000000 | (x)     |
| M3 Max  | t6031    |                         4.00 | 0x2a0400000 | ()      |
| M4      | t8132    |                         4.00 | 0x3c8000000 | ?       |
| M4 Pro  | t6040    |                         4.00 | 0x548700000 | ?       |
| A18 Pro | t8140    |                         4.00 | 0x348000000 | ?       |

### Credits

Thanks to Sven Peter for the earlier work and documentation on DebugUSB and Fiona Behrens for help with USB basics.
