// Programming the SL23 modem tester — one-page guide.
//
// Build:  typst compile programming-guide.typ

#let jitter-blue = rgb("#0891b2")
#let jitter-dark = rgb("#0f172a")
#let jitter-gray = rgb("#64748b")
#let jitter-rule = rgb("#e2e8f0")

#set document(title: "Programming the test board", author: "Jitter B.V.")

#set text(font: ("Helvetica Neue", "Helvetica", "Arial"), size: 11pt, fill: jitter-dark, lang: "en")
#set par(leading: 0.65em)
#show heading.where(level: 1): it => {
  v(12pt)
  block(text(size: 15pt, weight: "bold", fill: jitter-blue, it.body))
  v(4pt)
}
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
      [Jitter B.V. · SL23 modem tester · programming],
      context counter(page).display("1 / 1", both: true),
    )
  },
)

#block[
  #text(size: 20pt, weight: "bold")[Programming the test board]
  #v(-6pt)
  #text(size: 12pt, fill: jitter-gray)[
    Only needed once, or when Jitter supplies new firmware. After programming,
    the board runs the test on its own, with no PC.
  ]
]
#v(4pt)
#line(length: 100%, stroke: 1pt + jitter-blue)

= Equipment

#table(
  columns: (1fr, auto),
  [Item], [Setting],
  [Power supply via Würth 691361300002 or 691368300002B], [12.0 V, 1 A or more],
  [SWD programmer (J-Link, ST-LINK, or another programmer that supports the STM32L4R5VIT6)], [--],
  [PC with the flash program and the firmware file, both supplied by Jitter], [--],
)

= Connections

#figure(image("img/swd-programming.png", width: 92%))

= Steps

+ Connect the SWD programmer to *J5*, as in the drawing above.
+ Connect 12 V to the power connector.
+ Run the flash program on the PC and wait until it reports success.
+ Disconnect the programmer.

All LEDs light up briefly, then *Busy* starts blinking: the board is ready. It
keeps the firmware when the power is removed.

The test itself is described in the work instruction, _Modem factory test_.
