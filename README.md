# kisd

kisd is a daemon which implements parts of the KIS / DebugUSB protocol.
The main use at this time is getting accessing the "dockchannel uart" of Apple
Silicon machines from a device running Asahi Linux.


### Usage

- Start kisd
  - kisd will allocate a pseudo-terminal and create a symlink at /dev/m1n1
  - kisd will continuously scan for DebugUSB devices and attempt to attach the dockchannel uart to /dev/m1n1
- Connect your target device and put it into debugusb mode: `tuxvdmtool [reboot] debugusb` (TODO: integrate tuxvdmtool into kisd?)
- Run your m1n1 proxyclient commands against /dev/m1n1 as usual, or connect with picocom (baud rate doesn't matter)


### Base addresses

Currently it is not known how the correct write addresses to use in the DebugUSB messages for input / key presses are determined based on the previous handshake messages. If no `--base` is specified, kisd will attempt to guess the address in a way which works for some devices, but not all.

The following working base addresses have been determined based on Wireshark USB dumps of the DebugUSB communication under Mac OS.

| Device                              | Codename    | Protocol Version (bcdDevice) | Base        |
| ----------------------------------- | ----------- | ---------------------------- | ----------- |
| MacBook Air (M1, 2020)              | t8103-j313  |                         1.20 | 0x23d000000 |
| MacBook Pro (14-inch, M1 Pro, 2021) | t6000-j314s |                         1.20 | 0x292400000 |
| MacBook Air (13-inch, M2, 2022)     | t8112-j413  |                         2.00 | 0x23d000000 |
| Mac Mini (M4, 2024)                 | t8132-j773g |                         4.00 | 0x3c8000000 |
| Mac Mini (M4 Pro, 2024)             | t8132-j773s |                         4.00 | 0x548700000 |
| MacBook Neo (A18 Pro, 2026)         | t8140-j700  |                         4.00 | 0x348000000 |


### Credits

Thanks to Sven Peter for the earlier work and documentation on DebugUSB.
