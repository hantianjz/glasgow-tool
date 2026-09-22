"""Project-owned fixed UART wrapper around Glasgow's UART bit engine."""

from dataclasses import dataclass

from amaranth import Cat, Const, Elaboratable, Module, Signal
from amaranth.lib import stream, wiring
from amaranth.lib.fifo import SyncFIFOBuffered
from amaranth.lib.wiring import In, Out

from glasgow.abstract import GlasgowPin, GlasgowPort
from glasgow.gateware.uart import UART
from glasgow.hardware.assembly import HardwareAssembly


BAUD_DIVISOR_WIDTH = 20
COUNTER_WIDTH = 32
TX_FIFO_DEPTH = 512
TX_IDLE_BIT = 31


class FixedUARTComponent(wiring.Component):
    """8N1 UART with saturating counters and an observable transmit FIFO."""

    i_stream: In(stream.Signature(8))
    o_stream: Out(stream.Signature(8))

    baud_divisor: In(BAUD_DIVISOR_WIDTH)
    rx_errors: Out(COUNTER_WIDTH)
    rx_overflow: Out(COUNTER_WIDTH)
    tx_state: Out(32)

    def __init__(self, ports):
        self.ports = ports
        super().__init__()

    def elaborate(self, platform):
        del platform
        m = Module()
        m.submodules.uart = uart = UART(
            self.ports,
            bit_cyc=417,
            max_bit_cyc=(1 << BAUD_DIVISOR_WIDTH) - 1,
            data_bits=8,
            parity="none",
            stop_bits=1,
        )
        m.submodules.tx_fifo = tx_fifo = SyncFIFOBuffered(width=8, depth=TX_FIFO_DEPTH)

        m.d.comb += [
            uart.bit_cyc.eq(self.baud_divisor),
            tx_fifo.w_data.eq(self.i_stream.payload),
            tx_fifo.w_en.eq(self.i_stream.valid & tx_fifo.w_rdy),
            self.i_stream.ready.eq(tx_fifo.w_rdy),
            uart.tx_data.eq(tx_fifo.r_data),
            uart.tx_ack.eq(tx_fifo.r_rdy),
            tx_fifo.r_en.eq(uart.tx_rdy & tx_fifo.r_rdy),
            self.o_stream.payload.eq(uart.rx_data),
            self.o_stream.valid.eq(uart.rx_rdy),
            uart.rx_ack.eq(self.o_stream.ready),
            self.tx_state.eq(
                Cat(tx_fifo.level, Const(0, TX_IDLE_BIT - len(tx_fifo.level)),
                    uart.tx_rdy & ~tx_fifo.r_rdy)
            ),
        ]

        with m.If((uart.rx_ferr | uart.rx_perr) & (self.rx_errors != 0xFFFF_FFFF)):
            m.d.sync += self.rx_errors.eq(self.rx_errors + 1)
        with m.If(uart.rx_ovf & (self.rx_overflow != 0xFFFF_FFFF)):
            m.d.sync += self.rx_overflow.eq(self.rx_overflow + 1)

        return m


@dataclass(frozen=True)
class UARTResources:
    """Assembly objects whose allocated metadata is emitted in the manifest."""

    baud_divisor: object
    rx_errors: object
    rx_overflow: object
    tx_state: object
    pipe: object


def assemble(revision: str):
    """Assemble the fixed port-A profile for one revC stepping."""
    assembly = HardwareAssembly(revision=revision)
    assembly.use_voltage({GlasgowPort.A: 3.3})
    rx = GlasgowPin(GlasgowPort.A, 0)
    tx = GlasgowPin(GlasgowPort.A, 1)
    ports = assembly.add_port_group(rx=rx, tx=tx)
    assembly.use_pulls({rx: "high"})

    component = assembly.add_submodule(FixedUARTComponent(ports), name="native_uart")
    pipe = assembly.add_inout_pipe(
        component.o_stream,
        component.i_stream,
        out_fifo_depth=1,
    )
    resources = UARTResources(
        baud_divisor=assembly.add_rw_register(component.baud_divisor),
        rx_errors=assembly.add_ro_register(component.rx_errors),
        rx_overflow=assembly.add_ro_register(component.rx_overflow),
        tx_state=assembly.add_ro_register(component.tx_state),
        pipe=pipe,
    )
    return assembly, resources
