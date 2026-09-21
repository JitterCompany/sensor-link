# Modem test — work instruction

Product tested: LTE module on its mini-PCIe carrier.
Test time: about 15 seconds per module.

This instruction is for the test operator. The engineering description is in
[factory-test.md](factory-test.md).

---

## 1. Equipment

| Item | Setting |
|---|---|
| Test board (the tester) | — |
| Power supply | 12.0 V, able to give **1 A or more** |
| SIM card | any SIM card, 1 piece |
| Box A | modules that PASS |
| Box B | modules that FAIL |

The SIM card stays at the test station. It is used for every module.

---

## 2. Prepare the module

1. Put the SIM card in the module's SIM holder.
2. Put the module in the mini-PCIe slot. Press it down until the clips hold it.

## 3. Start the test

3. Press the **Reset** button.

All LEDs come on for a moment, then **Busy** starts blinking. That is the test
running.

**The 12 V stays connected all day.** Connect it once in the morning. You do not
disconnect power between modules — the Reset button runs the test again.

If the power supply has a current limit, set it to 1 A or higher. The board
draws far less than that while testing, but the moment power is first connected
it takes a short surge. A supply that limits at that moment can make good
modules look faulty.

## 4. Read the result

**Wait until Busy stops blinking.** That takes about 15 seconds. Do not read
the result before it stops.

Then look at the two LEDs marked **Modem test**, next to the module slot.

| Modem test | Result | Action |
|---|---|---|
| **Pass**, on and steady | **PASS** | Put the module in box A |
| **Fail**, blinking | **FAIL** | Go to section 5 |
| **Fail**, on and steady, not blinking | **Do not use this tester** | Go to section 6 |

To test the next module: wait for Busy to stop blinking, swap the module, press
**Reset** again.

**Only swap a module when Busy is not blinking.** While it blinks, the module
slot is powered. Swapping then can damage the module or the test board.

---

## 5. If Fail blinks

The **Fail** LED gives a group of flashes, then a long pause, then the same
group again. Count the flashes in one group. Count twice to be sure.

**The smallest number is always 2.** There is never a single flash.

| Flashes | Meaning | Action |
|---|---|---|
| 2 | Module does not answer | Box B |
| 3 | SIM card not detected | Reseat the SIM card and test again. Still 3 flashes → box B |
| 4 | Module uses too much power | Box B. Tell the engineer, keep this module separate |
| 5 | No module detected | Check the module is properly seated, then test again. Still 5 flashes → box B |
| 6 | Wrong module type | Box B. Tell the engineer, this is the wrong part |

---

## 6. If a red LED is on and does not blink

The test board itself has a fault. It did not test the module, and every **Fail**
LED on the board is on.

Do not put modules in box A or box B based on this board. Look at the **Status**
LEDs next to the power connector: **Error** blinks a number.

| Error flashes | Meaning |
|---|---|
| 2 | Processor fault |
| 3 | Charger fault |
| 4 | Power supply fault |
| 5 | Temperature fault |
| 6 | Firmware fault |

Note the number, set the test board aside and tell the engineer.

---

## 7. If nothing lights up at all

Check the 12 V supply and the connector. If the LEDs still do not come on when
power is connected, the test board is faulty — set it aside and tell the
engineer.

---

## Report sheet

| Date | Operator | Modules tested | Box A | Box B | Fault numbers seen |
|---|---|---|---|---|---|
| | | | | | |
