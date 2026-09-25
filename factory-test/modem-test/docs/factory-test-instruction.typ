// Modem factory test — work instruction for the test operator.
//
// Build:  typst compile factory-test-instruction.typ
//
// The engineering description is in factory-test.md.

#let jitter-blue = rgb("#0891b2")
#let jitter-dark = rgb("#0f172a")
#let jitter-gray = rgb("#64748b")
#let jitter-rule = rgb("#e2e8f0")

#set document(title: "Modem factory test — work instruction", author: "Jitter B.V.")

#set text(
  font: ("Helvetica Neue", "Helvetica", "Arial"),
  size: 11pt,
  fill: jitter-dark,
  lang: "en",
)
#set par(leading: 0.65em)

#show heading.where(level: 1): it => {
  v(14pt)
  block(text(size: 15pt, weight: "bold", fill: jitter-blue, it.body))
  v(4pt)
}
#show heading.where(level: 2): it => {
  v(10pt)
  block(text(size: 12pt, weight: "bold", fill: jitter-dark, it.body))
  v(2pt)
}

#set list(indent: 0.8em, marker: text(fill: jitter-blue, weight: "bold")[--])
#set enum(indent: 0.8em, spacing: 1.1em)
#set table(stroke: 0.5pt + jitter-rule, inset: 8pt)
#show table.cell.where(y: 0): set text(weight: "bold")

#set page(
  paper: "a4",
  margin: (top: 2.2cm, bottom: 1.8cm, left: 2.2cm, right: 2.2cm),
  footer: {
    line(length: 100%, stroke: 0.5pt + jitter-rule)
    v(5pt)
    set text(size: 8pt, fill: jitter-gray)
    grid(
      columns: (1fr, auto),
      [Jitter B.V. · SL23 modem tester · work instruction],
      context counter(page).display("1 / 1", both: true),
    )
  },
)

#block[
  #text(size: 20pt, weight: "bold")[Modem factory test]
  #v(-6pt)
  #text(size: 12pt, fill: jitter-gray)[
    Work instruction · LTE module on its mini-PCIe carrier · about 20 seconds per module
  ]
]
#v(4pt)
#line(length: 100%, stroke: 1pt + jitter-blue)

= 1. Equipment

#table(
  columns: (1fr, auto),
  [Item], [Setting],
  [Power supply via Würth 691361300002 or 691368300002B], [12.0 V, 1 A or more],
  [SWD programmer and a PC, to program the board], [first use only],
  [SIM card], [any SIM card, 1 piece, nano SIM format],
)

The SIM card stays at the test station. It is used for every module.

= 2. Power on the board

#figure(image("img/board-connections.png", width: 72%))

+ Connect 12 V to the power connector: *+* and *--* as marked on the board.
+ All LEDs light up briefly, then *Busy* starts blinking.

If they do not, the board still has to be programmed once: see
_Programming the test board_. After that the board works on its own, with no PC.

Leave the 12 V connected all day. The *Reset* button starts each new test.

= 3. Run the test

+ Wait until *Busy* stops blinking.
+ Put the SIM card in the modem, and the modem in the slot.
+ Press *Reset*. *Busy* starts blinking.
+ Wait until *Busy* stops blinking, which takes about 20 seconds.

#grid(
  columns: (auto, 1fr),
  column-gutter: 16pt,
  align: horizon,
  image("img/leds-status.png", width: 3.4cm),
  [
    *Busy blinking:* the test is running. Do not read the result, and do not
    remove the modem.

    *Error blinking:* the test board itself is broken. Its results cannot be
    trusted.
  ],
)

= 4. Read the result

Look at the two LEDs marked *Modem test*, next to the modem slot.

#grid(
  columns: (auto, 1fr),
  column-gutter: 16pt,
  align: horizon,
  image("img/leds-modem.png", width: 3.4cm),
  [
    *Pass on, does not blink:* the modem is good.

    *Fail blinking:* the modem is rejected. Count the flashes and write the
    number down.
  ],
)

#v(4pt)

The *Fail* LED gives a group of flashes, then a pause, then the same group
again. Count the flashes in one group. The smallest number is 2: there is never a single flash.

#table(
  columns: (auto, 1fr),
  [Flashes], [Meaning],
  [2], [Modem does not answer],
  [3], [SIM card not detected],
  [4], [Modem uses too much power],
  [5], [No modem detected (no power consumption)],
  [6], [Wrong modem type],
)

To test the next modem: wait until *Busy* has stopped blinking, swap the modem,
and press *Reset*.
