# FTDI C232HD-DDHSP-0: pinout and operating modes

This reference is for the **C232HD-DDHSP-0**, the **3.3 V** FT232H-based USB-to-UART cable. Do not substitute the pinout of the C232HM MPSSE cable, the C232HD-EDHSP-0 variant, or an FT232R cable.

The cable exposes eight digital signals plus power and ground through ten individual single-pole receptacles. Its nominal length is **1.8 m**, with a USB Type-A plug at the host end. UART is its advertised function, but PyFtdi explicitly documents this cable's wire colors for FT232H MPSSE and bit-bang operation.[1][2][11]

**Scope:** published wiring and software capabilities, not measurements of your individual cable. Alternative modes require appropriate host software and target wiring; changing wire connections alone does not switch modes.

## Electrical safety

- **3.3 V signalling with 5 V-tolerant inputs:** the cable datasheet specifies 3.3 V I/O cells that tolerate 5 V as inputs. Outputs are still 3.3 V logic, not 5 V, and may not meet a 5 V target's input-high threshold. This is not a variable-voltage adapter. Use level translation for a 1.8 V target; the cable's specified input-high minimum is 2.0 V. Power-off input tolerance is not established here.[11]
- **Red is a power output, not a voltage-reference input:** this model supplies **3.3 V, up to 250 mA**. Normally leave red disconnected when the target has its own supply. Never connect two supply outputs together unless the power design explicitly permits it.[1]
- **Black is common ground:** connect it to target ground for every interface. The cable does not provide galvanic isolation.
- **Not RS-232 electrical signalling:** do not connect directly to a conventional positive/negative-voltage RS-232 port. RS-232, RS-485, CAN, and similar buses need appropriate external transceivers; a transceiver alone does not implement a protocol controller.
- Digital outputs are logic signals, not load power. The documented drive-strength settings are 4/8/12/16 mA, not a claim about this cable's current EEPROM setting or a safe aggregate load. Use a resistor for an LED and a driver for a relay, motor, or other substantial load. The 250 mA supply rating is not a GPIO rating.[11]
- Avoid driving an unpowered target: signal wires can back-power it through protection structures even when red is disconnected.
- Disconnect the target before changing modes. A wire that was an input may become an output. **The FT232H initially starts in UART mode**, so UART outputs can drive before the application configures MPSSE/GPIO, including after a USB reset. Initialize safe output levels before reconnecting, and design against startup contention rather than relying on a script running quickly.[12]
- Treat advertised clock rates as hardware ceilings, not guaranteed working rates through flying leads. Start with a low bus speed and increase only after checking the target's limits and signal integrity.

## Physical wire mapping

Use **wire color** to identify each individual lead, not its position after arranging the loose connectors. FTDI's numbered cable drawing refers to **internal PCB pads 1–10**, not a fixed ten-position external header. `ADBUSn` is the FT232H signal name; `ADn`/`Dn` are common abbreviations. PCB pad numbers, IC package pin numbers, and GPIO bit numbers are different identifiers.[11][12]

`I` = input **to the cable**, `O` = output **from the cable**, `I/O` = software-selected direction. `#` means active low. UART names and directions apply in UART mode, not permanently to the wire.

| PCB pad | Wire color | FT232H signal | IC pin | GPIO bit / mask | UART function | UART direction |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Red | Power supply | — | Not GPIO | +3.3 V supply output | O, power |
| 2 | Orange | ADBUS0 | 13 | 0 / `0x01` | TXD: transmit data | O |
| 3 | Yellow | ADBUS1 | 14 | 1 / `0x02` | RXD: receive data | I |
| 4 | Green | ADBUS2 | 15 | 2 / `0x04` | RTS#: request to send | O |
| 5 | Brown | ADBUS3 | 16 | 3 / `0x08` | CTS#: clear to send | I |
| 6 | Grey | ADBUS4 | 17 | 4 / `0x10` | DTR#: data terminal ready | O |
| 7 | Purple | ADBUS5 | 18 | 5 / `0x20` | DSR#: data set ready | I |
| 8 | White | ADBUS6 | 19 | 6 / `0x40` | DCD#: data carrier detect | I |
| 9 | Blue | ADBUS7 | 20 | 7 / `0x80` | RI#: ring indicator | I |
| 10 | Black | Ground | — | Not GPIO | GND | Reference |

FTDI's cable schematic connects all eight ADBUS signals directly to these pads, without external level shifters or directional buffers in the signal paths. PyFtdi's **C232HD cable** column independently agrees with the mapping. Grey may also be written “gray.”[2][11]

In the chip's MPSSE terminology, **GPIOL0–GPIOL3 mean ADBUS4–ADBUS7**, not GPIO bits 0–3. Thus GPIOL3/RTCK is **blue, bit 7**. No ACBUS pins are exposed at the cable end.[11][12]

**Important:** yellow changes from UART **input** to SPI/JTAG **output**. Green changes from UART **output** to SPI/JTAG **input**. Do not preserve UART TX/RX wiring when switching protocols.

## Mode selection and typical applications

| Mode | What it does | Typical uses | Host software |
| --- | --- | --- | --- |
| UART | Full-duplex asynchronous serial, with optional handshaking | Boot consoles, logs, device commands, supported serial bootloaders | OS virtual serial port plus a terminal or `pyserial`; FTDI D2XX |
| SPI master via MPSSE | Generates clock and transfers serial data to/from a peripheral | SPI flash access, sensors, ADC/DAC register access, display controllers | PyFtdi `SpiController`; compatible FTDI programming tools |
| I²C master via MPSSE | Addresses devices on a shared two-wire bus | Sensors, EEPROMs, RTCs, GPIO expanders | PyFtdi `I2cController` |
| JTAG via MPSSE | Drives a JTAG scan chain | IDCODE scans, boundary scan, supported MCU debugging and FPGA programming | OpenOCD's `ftdi` driver or a compatible programmer |
| Asynchronous bit-bang GPIO | Independently reads/drives up to eight digital wires | Buttons, status lines, reset/enable control, simple output patterns | PyFtdi `GpioAsyncController` |
| Synchronous bit-bang GPIO | Couples each output-byte update to an input sample | Clocked custom digital exchanges and short stimulus/response sequences | PyFtdi `GpioSyncController` |
| MPSSE GPIO | Reads/drives GPIO with MPSSE commands; unused pins can accompany a bus | Chip selects, reset lines, interrupt inputs alongside SPI/I²C | PyFtdi `GpioMpsseController`, or the bus controller's `get_gpio()` |
| Fast Serial, conditional | FTDI-specific externally clocked serial protocol | Custom FPGA/logic links; external isolation designs | EEPROM mode selection plus protocol-aware software; see limitations below |

**One FT232H interface, one active mode:** UART and MPSSE are not independent simultaneous ports. SPI and I²C can share their unused pins with GPIO, but not with a second UART. Do not open the same cable concurrently from a serial terminal and an MPSSE program.[3][4][5]

## Every wire in the common modes

SPI below uses one chip select on ADBUS3. JTAG uses a conventional four-signal scan chain. GPIO direction must be explicitly configured.

| Wire | UART | SPI master | I²C master | JTAG | GPIO-only modes |
| --- | --- | --- | --- | --- | --- |
| Red | +3.3 V power O | Same | Same | Same | Same; never GPIO |
| Black | GND | GND | GND | GND | GND |
| Orange | TXD O | SCLK O | SCL open-drain O | TCK O | GPIO0 I/O |
| Yellow | RXD I | MOSI O | SDA output, open-drain | TDI O | GPIO1 I/O |
| Green | RTS# O | MISO I | SDA input I | TDO I | GPIO2 I/O |
| Brown | CTS# I | CS0# O | GPIO3 I/O | TMS O | GPIO3 I/O |
| Grey | DTR# O | GPIO4, or extra CS# O | GPIO4 I/O | Optional GPIO4 / reset | GPIO4 I/O |
| Purple | DSR# I | GPIO5, or extra CS# O | GPIO5 I/O | Optional GPIO5 / reset | GPIO5 I/O |
| White | DCD# I | GPIO6, or extra CS# O | GPIO6 I/O | Optional GPIO6 | GPIO6 I/O |
| Blue | RI# I | GPIO7, or extra CS# O | GPIO7, or SCL feedback I | Optional RTCK I, otherwise GPIO7 | GPIO7 I/O |

Extra SPI chip selects and JTAG reset assignments are **software choices**, not permanently wired cable functions. A library may reserve more pins or expose fewer optional functions than the chip supports. Red and black retain their electrical functions in every mode.[2][3][4][6]

## UART: serial console and bootloader access

Minimum connections:

| Cable lead | Target connection |
| --- | --- |
| Orange, TXD O | Target RX |
| Yellow, RXD I | Target TX |
| Black | Target GND |
| Red | Leave disconnected if target is independently powered |

For hardware flow control, connect **green RTS# to target CTS#** and **brown CTS# to target RTS#**, provided the target uses compatible active-low logic-level handshaking. Otherwise disable hardware flow control in the terminal and leave those leads unused.

Grey DTR# can control a target reset or boot-selection circuit if that board was designed for it. Opening a serial port may change DTR/RTS; do not connect them blindly to sensitive signals. Purple DSR#, white DCD#, and blue RI# are modem-status inputs and can remain disconnected when not used.

Blue RI# can also request USB remote wakeup when that EEPROM option and host support are enabled. The cable's documented factory configuration disables remote wakeup; it is not an always-active wake button.[11]

Typical starting setup: the **target's documented baud rate**, commonly 115200, with 8 data bits, no parity, and 1 stop bit (`8N1`). Firmware upload works only with a compatible target bootloader and the correct boot/reset sequence; this cable is not a universal programmer.

The cable is rated for **up to 12 Mbaud**, with 7/8 data bits, 1/2 stop bits, and selectable parity. Not every baud rate is achievable, and software may impose a lower limit. For example, PyFtdi documents UART support up to 6 Mbps. A 12 Mbaud line rate is not 12 MB/s of payload.[7][11][12]

A useful disconnected-target check is a UART loopback: join **orange to yellow**, leave the other signal leads unconnected, disable local echo and hardware flow control, and confirm transmitted bytes return. This checks UART TX/RX, not every wire or mode.

## SPI master: flash and peripheral access

Connect **orange → SCLK**, **yellow → MOSI/SDI**, **green ← MISO/SDO**, **brown → CS#**, and **black → GND**.

With PyFtdi, `cs=0` uses brown/ADBUS3. Reserving additional chip selects assigns them consecutively to grey/ADBUS4, purple/ADBUS5, white/ADBUS6, and blue/ADBUS7; reserved chip-select pins are no longer free GPIO.[2][3]

Typical usage: read a serial flash's JEDEC ID, inspect a sensor register, or update an output device. Use the peripheral's documented SPI mode and maximum frequency. Keep other masters inactive when accessing an in-circuit flash, and handle its write-protect/hold pins according to its datasheet. Reading an ID is preferable to writing or erasing for the first check.

Limits:

- This is an **SPI master**, not a general-purpose SPI slave or passive SPI sniffer.
- PyFtdi documents modes 0 and 2 as supported, with workarounds and timing caveats for modes 1 and 3. Check the software's actual support rather than assuming all four modes behave identically.[3]
- USB round trips introduce gaps and jitter between operations. A precise peripheral clock does not make host-triggered sampling or command timing deterministic.
- The FT232H's MPSSE clock ceiling is **30 MHz**. The C232HD UART cable datasheet does not guarantee that SPI/JTAG will operate at that rate over the complete 1.8 m cable.[11][12]

## I²C master: sensors and EEPROMs

**Join yellow and green together at the target's SDA net.** They are separate FT232H transmit and receive pins implementing one bidirectional bus signal. Orange drives SCL; black supplies the common reference.[4]

```text
Cable                                  Target I²C bus
Orange / ADBUS0 ----------------------- SCL
Yellow / ADBUS1 --------+-------------- SDA
Green  / ADBUS2 --------+
Black -------------------------------- GND

SCL --- pull-up resistor --- target 3.3 V rail
SDA --- pull-up resistor --- target 3.3 V rail
```

- Provide SCL/SDA pull-ups to the **compatible target logic supply**, not automatically to the cable's red lead. Existing board pull-ups may suffice. A value such as 4.7 kΩ is a starting point, not a universal specification; check capacitance, speed, and sink-current requirements.
- FT232H supports real **drive-low/release-high** operation. PyFtdi automatically enables this for I²C SCL and SDA output. Do not tie yellow/green together while the cable is configured for a conflicting UART or GPIO output mode.[4][5]
- Start at **100 kHz** for an ordinary standard-mode target, or slower if required by wiring/target constraints. Bus frequency is not the same as sustained host data rate: ACK handling and USB latency reduce throughput.
- If the target stretches the clock, enable PyFtdi's `clockstretching` option and additionally connect **blue/ADBUS7 to SCL** alongside orange. Blue is then a feedback input, not spare GPIO. The diode workaround documented for FT2232H/FT4232H is **not required for FT232H**.[4]
- Clock-stretch handling is a library-specific use of adaptive clocking, **not an unconditional MPSSE I²C guarantee**. Validate it with the actual target. FTDI AN_355 discusses this limitation; its ADBUS5 feedback reference conflicts with the FT232H pin table and PyFtdi's ADBUS7 wiring. This reference follows **PyFtdi's documented blue/ADBUS7 connection**, not that conflicting line.[4][12][13]
- Brown, grey, purple, and white remain available as GPIO; blue is also available when clock-stretch feedback is not used.
- Use this as a single I²C master, not a general-purpose I²C slave or guaranteed multi-master controller. PyFtdi's documented addressing support is 7-bit.[7]

Typical usage: read a temperature register or EEPROM contents using the target's documented address and transaction format. Avoid indiscriminate probing of an unfamiliar live bus: some devices have side effects even on apparently simple transactions.

## JTAG: scan chains, programming, and debugging

| Cable lead | Target signal | Direction relative to cable |
| --- | --- | --- |
| Orange | TCK | O |
| Yellow | TDI | O |
| Green | TDO | I |
| Brown | TMS | O |
| Black | GND | Reference |
| Blue, optional | RTCK / returned clock | I, only when adaptive clocking is configured |
| Grey / purple / white | Optional reset/control signals selected by adapter configuration | Configuration-dependent |
| Red | Optional target power only; **not VTref** | O, power |

JTAG names describe the **target** signals: cable TDI goes to target TDI, not target TDO. Unlike UART TX/RX, do not cross TDI/TDO.

Typical usage: scan IDCODEs or a boundary-scan chain; program or debug a supported device using a matching tool and target configuration. OpenOCD supports FT232H MPSSE through its **`ftdi` driver**, not its separate `ft232r` bit-bang driver.[6]

There are **no fixed TRST#/SRST# wires** on this cable. For example, software could assign grey/ADBUS4 to one reset and purple/ADBUS5 to another, but the adapter configuration and target electrical requirements must agree. A shared active-low reset may require open-drain or tri-state control rather than a push-pull output. PyFtdi's JTAG support is low-level; it is not by itself a complete MCU programmer/debugger.[6][7]

The cable has no target-voltage sensing/reference input. Do not mistake the red power output for the VTref connection on a standard debug header. Use proper level translation for targets that are not compatible with 3.3 V signalling.

## GPIO and bit-bang: eight reusable digital signals

All eight signal wires become GPIO0–GPIO7 using the physical mapping above. Their UART directions no longer apply. In a direction mask, **1 means output, 0 means input**; in a value mask, each bit controls or reports the corresponding electrical level.[5]

Examples of usage:

- **Asynchronous GPIO:** read a pushbutton/status input; drive an enable or reset through suitable circuitry; send a buffered digital output pattern. Input sampling runs independently until the receive buffer fills, so buffered reads may contain old samples. PyFtdi's peek read is appropriate when the current pin state is wanted.
- **Synchronous bit-bang:** send a sequence of eight-bit output values and receive a corresponding sequence of input samples. The chip samples **before applying the next output byte**, so input is one byte behind the output sequence; append another output byte to capture the response to the last state. All-input capture still requires dummy writes. This is not a timestamped logic-analyzer capture.[12]
- **MPSSE GPIO:** use low-byte GPIO commands for control signals. During SPI or I²C, obtain the GPIO interface from the active bus controller instead of opening a second GPIO controller on the same cable.

For example, mask `0x10` selects **grey/ADBUS4**, not the fourth lead in a connector arrangement. The cable's eight signal bits occupy `0xFF`; software exposing a wider FT232H port does not imply that the cable has those extra wires.

USB latency prevents precise timing of separate host GPIO calls. Buffered patterns have hardware pacing, but buffering and transfer boundaries still matter. This cable is not an oscilloscope, analog input device, general PWM controller, or replacement for a triggered logic analyzer.

## Fast Serial: an uncommon, conditional chip mode

**[INFERENCE]** The exposed ADBUS wires contain the complete four-signal data/clock interface for FTDI Fast Serial. This is a possible custom-hardware use, **not a verified plug-and-play operating mode of this individual cable**. It requires EEPROM selection, compatible target logic, and suitable handling of the internal, unexposed SIWU# input. Check those conditions before attempting it.[11][12]

This protocol is **not UART or SPI**. The external target supplies its clock; data is LSB-first with FTDI-specific framing. The target may transmit into the FT232H when FSCTS is **high**, per the datasheet's functional description.

| Wire | Fast Serial function | Direction / use |
| --- | --- | --- |
| Red | Supply output | Same electrical power connection; do not assume availability after arbitrary EEPROM changes |
| Black | GND | Common reference for a direct, non-isolated connection |
| Orange / ADBUS0 | FSDI | I: data from target |
| Yellow / ADBUS1 | FSCLK | I: clock supplied by target |
| Green / ADBUS2 | FSDO | O: data to target |
| Brown / ADBUS3 | FSCTS | O: permits target transmission when high |
| Grey / ADBUS4 | Unused | Tri-stated with internal pull-up; not spare GPIO in this mode |
| Purple / ADBUS5 | Unused | Tri-stated with internal pull-up |
| White / ADBUS6 | Unused | Tri-stated with internal pull-up |
| Blue / ADBUS7 | Unused | Tri-stated with internal pull-up |

Typical use is a custom FPGA/logic link implementing FTDI's protocol, potentially through **external** optocouplers/digital isolators and separately isolated power. “Opto-isolated serial mode” does **not** mean the cable itself provides isolation; directly joining grounds or supplies across an isolation barrier defeats it.

Unlike MPSSE, this mode is selected in the EEPROM. D2XX bit-mode value `0x10` merely holds an already-selected Fast Serial controller in reset; `0x00` releases it. Neither substitutes for EEPROM selection. Do not treat the chip's Fast Serial timing limit as a tested cable operating rate.[12]

## Chip features that the cable does not expose

The FT232H has more pins than this cable brings out. **[INFERENCE]** Combining the cable schematic with the chip's required pin assignments rules out these native interfaces at the unmodified cable end:[11][12]

| Native chip feature | What the signal wires would carry | Missing connections / limitation |
| --- | --- | --- |
| FT245 asynchronous FIFO | ADBUS0–7 = D0–7, in the same color order as GPIO | Requires ACBUS0–3 for RXF#, TXE#, RD#, WR# |
| FT245 synchronous FIFO | ADBUS0–7 = D0–7 | Requires those control pins plus ACBUS5 CLKOUT and ACBUS6 OE# |
| CPU-style FIFO | ADBUS0–7 = D0–7 | Requires ACBUS0–3 for CS#, A0, RD#, WR# |
| FT1248, including one-bit width | Selected ADBUS pins = MIOSIO data bits | Requires ACBUS0 SCLK, ACBUS1 SS_n, ACBUS2 MISO/status |
| ACBUS GPIO / dedicated clocks / UART TXDEN | Not functions of the eight external signal wires | Required ACBUS pins are internal; no extra eight-wire GPIO bank or dedicated RS-485 direction output |
| Bit-bang external read/write strobes | ADBUS0–7 remain usable as ordinary bit-bang data | Dedicated RDSTB#/WRSTB# are on ACBUS and unavailable externally |

Red remains the physical supply lead and black remains ground; neither can stand in for a missing control signal. Reprogramming the EEPROM cannot bring absent ACBUS wires out or remap a native hardware interface onto arbitrary pins. Internal ACBUS lines also serve cable circuitry; avoid blindly driving the high GPIO byte.[11][12]

**SWD is not the same as JTAG.** OpenOCD can implement SWD with suitable FTDI adapter wiring and configuration, including bidirectional SWDIO handling, but the C232HD documentation does not define a ready-made SWD cable pinout. Do not directly reuse the JTAG or joined-wire I²C wiring for SWD. A purpose-built SWD adapter is the simpler option.[6]

## Software setup and mode changes

- **UART:** use an FTDI virtual COM-port driver and a serial application. Select the actual detected device, such as a macOS `/dev/cu.usbserial-*` port, rather than guessing a device name.
- **MPSSE/GPIO:** PyFtdi uses PyUSB/libusb. Its documented selector for one ordinary FT232H is `ftdi://ftdi:232h/1`; select by actual serial number when multiple devices are present. PyFtdi's installation guide includes this exact cable in its device-enumeration example.[8][9]
- **JTAG:** OpenOCD needs an FTDI adapter configuration matching these wire assignments plus a configuration for the actual target.[6]
- Close the previous application's device handle before switching modes. A driver binding can prevent direct USB access; follow the selected tool's OS-specific instructions instead of changing unrelated USB drivers.
- Normal UART/MPSSE/bit-bang selection is performed by host software. Do not rewrite the EEPROM merely to use SPI, I²C, JTAG, or ordinary GPIO. Back up the EEPROM before any deliberate persistent reconfiguration; incorrect settings can make the device inaccessible.[10]

## Sources

1. [FTDI C232HD-DDHSP-0 product specifications](https://ftdichip.com/products/c232hd-ddhsp-0/) and [C232HD UART cable datasheet](https://www.ftdichip.com/Support/Documents/DataSheets/Cables/DS_C232HD_UART_CABLE.pdf).
2. [PyFtdi: FTDI device pinout](https://eblot.github.io/pyftdi/pinout.html), including the explicit **C232HD cable** color column.
3. [PyFtdi: SPI API, GPIO allocation, modes, and limitations](https://eblot.github.io/pyftdi/api/spi.html).
4. [PyFtdi: I²C API, wiring, open-drain operation, and clock stretching](https://eblot.github.io/pyftdi/api/i2c.html).
5. [PyFtdi: GPIO concepts](https://eblot.github.io/pyftdi/gpio.html) and [GPIO controller APIs](https://eblot.github.io/pyftdi/api/gpio.html).
6. [OpenOCD: debug adapter configuration](https://openocd.org/doc/html/Debug-Adapter-Configuration.html), especially the `ftdi` interface driver and reset/signal definitions.
7. [PyFtdi: supported features](https://eblot.github.io/pyftdi/features.html).
8. [PyFtdi: installation and device enumeration](https://eblot.github.io/pyftdi/installation.html).
9. [PyFtdi: device URL selection](https://eblot.github.io/pyftdi/urlscheme.html).
10. [PyFtdi: EEPROM management and recovery warnings](https://eblot.github.io/pyftdi/eeprom.html).
11. **Cable primary source:** FTDI *C232HD USB 2.0 Hi-Speed to UART Cable Datasheet*, v1.3, FT_000430, reproduced by AllDataSheet: [document index](https://www.alldatasheet.net/datasheet-pdf/pdf/1244741/FTDI/C232HD.html), [p3 model specifications](https://www.alldatasheet.net/html-pdf/1244741/FTDI/C232HD/344/3/C232HD.html), [p6 UART features](https://www.alldatasheet.co.uk/html-pdf/1244741/FTDI/C232HD/695/6/C232HD.html), [p7 PCB-pad numbering](https://www.alldatasheet.com/html-pdf/1244741/FTDI/C232HD/812/7/C232HD.html), [p9 wire table and supply ratings](https://www.alldatasheet.com/html-pdf/1244741/FTDI/C232HD/1046/9/C232HD.html), [p10 logic levels](https://www.alldatasheet.com/html-pdf/1244741/FTDI/C232HD/1163/10/C232HD.html), [p11 5 V input tolerance](https://www.alldatasheet.fr/html-pdf/1244741/FTDI/C232HD/1280/11/C232HD.html), [p12 DDHSP schematic](https://www.alldatasheet.com/html-pdf/1244741/FTDI/C232HD/1397/12/C232HD.html), and [p15 factory EEPROM configuration](https://www.alldatasheet.com/html-pdf/1244741/FTDI/C232HD/1748/15/C232HD.html).
12. **Chip primary source:** FTDI [*FT232H Single Channel Hi-Speed USB to Multipurpose UART/FIFO IC Datasheet*, v2.2, FT_000288](https://strawberry-linux.com/pub/DS_FT232H.pdf), distributor-hosted copy. See printed pp9–10 for the complete mode matrix and startup states, pp14–19 for signal descriptions, pp31–32 for bit-bang timing, pp33–34 for MPSSE/adaptive clocking, pp35–37 for Fast Serial, and p41 for mode selection.
13. **I²C primary source:** FTDI [*AN_355: FT232H MPSSE Example— I²C Master with Visual Basic*, v1.0](https://datasheet.datasheetarchive.com/originals/crawler/ftdichip.com/6689e39fbb7483d1001ca68e09391f41.pdf), especially p7 wiring/startup, p17 drive-only-zero, and p28 clock-stretch limitations. Its example cable is C232HM; only the FT232H protocol discussion is used here, **not its cable pinout**.

