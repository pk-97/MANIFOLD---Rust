# BUG-046 P6a sweep tables — 2026-07-06 (instrument: crates/manifold-audio/examples/hpss_proto.rs)

Replica validated fire-count-exact vs mod_harness on all 25 fixtures x 4 bands before any sweep.
Cells: <track>:<recovered±35ms>(<±70ms>)/<drums-Low-GT> sp<spurious>. Guards line = offline replay of the six fire-gated selftest scenarios.

## Round 1 — column masks: Sub (S), Gate (G), Wiener (W)
```
== SWEEP ==
guards gate: dive F=0,L=0 | riser F=0 | growl F=0 | kicks L==8 | busymix L>=7 | densemix L>=6
baseline       | guards FAIL (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5/13 sp0  bad_:0/45 sp4  feel:4/35 sp2  inha:17/22 sp2  tear:8/25 sp4 | drums-ret 1.00
S a=1.0 H=16   | guards FAIL (dF0 dL3 rF0 gF0 k8 b18 d9) | apri:9/13 sp5  bad_:13/45 sp22  feel:8/35 sp27  inha:18/22 sp6  tear:8/25 sp4 | drums-ret 0.97
S a=1.5 H=16   | guards FAIL (dF0 dL2 rF0 gF32 k8 b14 d23) | apri:13/13 sp10  bad_:16/45 sp31  feel:29/35 sp51  inha:20/22 sp10  tear:17/25 sp12 | drums-ret 0.97
S a=2.0 H=16   | guards FAIL (dF2 dL2 rF1 gF64 k13 b14 d19) | apri:13/13 sp8  bad_:18/45 sp30  feel:31/35 sp39  inha:21/22 sp11  tear:18/25 sp14 | drums-ret 0.97
G b=1.5 H=16   | guards FAIL (dF0 dL6 rF0 gF16 k10 b22 d29) | apri:13/13 sp47  bad_:17/45 sp33  feel:23/35 sp57  inha:21/22 sp13  tear:18/25 sp16 | drums-ret 1.00
G b=2.0 H=16   | guards FAIL (dF4 dL4 rF1 gF46 k12 b29 d28) | apri:13/13 sp13  bad_:19/45 sp36  feel:30/35 sp59  inha:21/22 sp16  tear:19/25 sp20 | drums-ret 1.00
G b=3.0 H=16   | guards FAIL (dF7 dL2 rF15 gF73 k12 b15 d19) | apri:13/13 sp7  bad_:18/45 sp44  feel:31/35 sp41  inha:21/22 sp14  tear:18/25 sp17 | drums-ret 0.97
W H=16 P=6     | guards FAIL (dF1 dL2 rF0 gF0 k8 b10 d3) | apri:7/13 sp0  bad_:4/45 sp5  feel:3/35 sp7  inha:17/22 sp5  tear:7/25 sp6 | drums-ret 1.00
W H=16 P=12    | guards FAIL (dF1 dL2 rF0 gF0 k8 b12 d2) | apri:6/13 sp0  bad_:5/45 sp9  feel:3/35 sp7  inha:18/22 sp6  tear:7/25 sp7 | drums-ret 1.00
W H=16 P=18    | guards FAIL (dF1 dL2 rF0 gF0 k8 b12 d3) | apri:7/13 sp1  bad_:7/45 sp9  feel:2/35 sp11  inha:17/22 sp7  tear:6/25 sp5 | drums-ret 0.94
S a=1.0 H=32   | guards FAIL (dF0 dL2 rF0 gF8 k8 b18 d12) | apri:8/13 sp7  bad_:13/45 sp23  feel:8/35 sp23  inha:16/22 sp5  tear:9/25 sp11 | drums-ret 0.95
S a=1.5 H=32   | guards FAIL (dF1 dL0 rF1 gF32 k8 b13 d21) | apri:12/13 sp7  bad_:17/45 sp34  feel:26/35 sp39  inha:20/22 sp12  tear:17/25 sp13 | drums-ret 0.94
S a=2.0 H=32   | guards FAIL (dF5 dL0 rF1 gF47 k8 b12 d18) | apri:13/13 sp1  bad_:17/45 sp39  feel:26/35 sp38  inha:20/22 sp12  tear:18/25 sp5 | drums-ret 0.94
G b=1.5 H=32   | guards FAIL (dF0 dL0 rF1 gF18 k7 b31 d29) | apri:10/13 sp44  bad_:19/45 sp33  feel:19/35 sp57  inha:20/22 sp19  tear:19/25 sp15 | drums-ret 0.97
G b=2.0 H=32   | guards FAIL (dF6 dL0 rF0 gF38 k7 b21 d22) | apri:12/13 sp6  bad_:20/45 sp46  feel:28/35 sp73  inha:20/22 sp15  tear:18/25 sp14 | drums-ret 0.94
G b=3.0 H=32   | guards FAIL (dF7 dL0 rF12 gF56 k8 b13 d17) | apri:13/13 sp4  bad_:20/45 sp42  feel:30/35 sp50  inha:21/22 sp14  tear:17/25 sp5 | drums-ret 0.94
W H=32 P=6     | guards FAIL (dF1 dL3 rF0 gF0 k8 b11 d4) | apri:6/13 sp0  bad_:4/45 sp8  feel:5/35 sp6  inha:18/22 sp5  tear:9/25 sp6 | drums-ret 1.00
W H=32 P=12    | guards FAIL (dF2 dL1 rF0 gF0 k8 b13 d2) | apri:6/13 sp0  bad_:9/45 sp10  feel:4/35 sp6  inha:18/22 sp7  tear:9/25 sp7 | drums-ret 0.97
W H=32 P=18    | guards FAIL (dF2 dL1 rF0 gF0 k8 b15 d3) | apri:7/13 sp1  bad_:9/45 sp15  feel:2/35 sp8  inha:18/22 sp6  tear:8/25 sp8 | drums-ret 0.94
S a=1.0 H=64   | guards FAIL (dF0 dL0 rF0 gF16 k8 b20 d15) | apri:9/13 sp2  bad_:11/45 sp23  feel:8/35 sp27  inha:17/22 sp6  tear:9/25 sp8 | drums-ret 1.00
S a=1.5 H=64   | guards FAIL (dF1 dL0 rF1 gF25 k8 b11 d17) | apri:12/13 sp4  bad_:18/45 sp33  feel:19/35 sp48  inha:20/22 sp11  tear:17/25 sp9 | drums-ret 1.00
S a=2.0 H=64   | guards FAIL (dF4 dL0 rF1 gF37 k8 b10 d18) | apri:13/13 sp0  bad_:20/45 sp39  feel:28/35 sp35  inha:21/22 sp15  tear:18/25 sp8 | drums-ret 1.00
G b=1.5 H=64   | guards FAIL (dF1 dL1 rF1 gF24 k7 b34 d23) | apri:12/13 sp32  bad_:13/45 sp38  feel:17/35 sp62  inha:21/22 sp16  tear:16/25 sp16 | drums-ret 1.00
G b=2.0 H=64   | guards FAIL (dF4 dL0 rF0 gF33 k7 b20 d20) | apri:12/13 sp4  bad_:24/45 sp54  feel:25/35 sp74  inha:21/22 sp21  tear:17/25 sp12 | drums-ret 1.00
G b=3.0 H=64   | guards FAIL (dF6 dL0 rF11 gF49 k8 b10 d19) | apri:13/13 sp0  bad_:16/45 sp38  feel:29/35 sp28  inha:20/22 sp21  tear:18/25 sp6 | drums-ret 0.97
W H=64 P=6     | guards FAIL (dF2 dL0 rF0 gF0 k8 b16 d5) | apri:6/13 sp0  bad_:5/45 sp6  feel:4/35 sp7  inha:18/22 sp4  tear:7/25 sp4 | drums-ret 1.00
W H=64 P=12    | guards FAIL (dF2 dL0 rF0 gF0 k8 b17 d5) | apri:6/13 sp0  bad_:10/45 sp10  feel:4/35 sp7  inha:18/22 sp5  tear:8/25 sp6 | drums-ret 1.00
W H=64 P=18    | guards FAIL (dF2 dL0 rF0 gF0 k8 b16 d4) | apri:7/13 sp2  bad_:11/45 sp11  feel:3/35 sp9  inha:17/22 sp6  tear:7/25 sp7 | drums-ret 1.00

== RETENTION (Low-band fires per clip, baseline first) ==
```

## Round 2 — dB novelty floor as replacement ODF (N)
```
== SWEEP ==
guards gate: dive F=0,L=0 | riser F=0 | growl F=0 | kicks L==8 | busymix L>=7 | densemix L>=6
baseline       | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:0(0)/45 sp4  feel:4(6)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
N m=3.0 H=16   | guards FAIL (dF0 dL0 rF0 gF73 k7 b10 d8) | apri:12(12)/13 sp0  bad_:12(20)/45 sp25  feel:24(25)/35 sp13  inha:17(18)/22 sp5  tear:14(18)/25 sp4 | drums-ret 0.84
N m=4.5 H=16   | guards FAIL (dF0 dL0 rF0 gF73 k7 b8 d9) | apri:12(12)/13 sp0  bad_:12(21)/45 sp26  feel:24(25)/35 sp10  inha:17(19)/22 sp5  tear:14(17)/25 sp5 | drums-ret 0.76
N m=6.0 H=16   | guards FAIL (dF0 dL0 rF0 gF73 k7 b8 d9) | apri:12(12)/13 sp0  bad_:10(15)/45 sp26  feel:22(24)/35 sp10  inha:18(19)/22 sp5  tear:14(16)/25 sp3 | drums-ret 0.68
N m=9.0 H=16   | guards FAIL (dF0 dL0 rF0 gF73 k7 b7 d7) | apri:10(11)/13 sp0  bad_:8(13)/45 sp19  feel:19(21)/35 sp5  inha:18(18)/22 sp3  tear:14(15)/25 sp1 | drums-ret 0.48
N m=3.0 H=32   | guards FAIL (dF0 dL0 rF1 gF62 k7 b8 d9) | apri:13(13)/13 sp0  bad_:16(21)/45 sp23  feel:19(20)/35 sp12  inha:17(18)/22 sp5  tear:13(15)/25 sp2 | drums-ret 0.84
N m=4.5 H=32   | guards FAIL (dF0 dL0 rF0 gF73 k7 b8 d8) | apri:13(13)/13 sp0  bad_:13(17)/45 sp20  feel:22(23)/35 sp8  inha:17(18)/22 sp4  tear:14(15)/25 sp2 | drums-ret 0.76
N m=6.0 H=32   | guards FAIL (dF1 dL0 rF0 gF73 k7 b8 d8) | apri:12(12)/13 sp0  bad_:10(18)/45 sp20  feel:25(27)/35 sp5  inha:17(17)/22 sp3  tear:14(15)/25 sp3 | drums-ret 0.68
N m=9.0 H=32   | guards FAIL (dF0 dL0 rF0 gF73 k7 b7 d7) | apri:10(11)/13 sp0  bad_:6(11)/45 sp18  feel:20(22)/35 sp3  inha:18(18)/22 sp1  tear:14(15)/25 sp1 | drums-ret 0.48
N m=3.0 H=64   | guards FAIL (dF0 dL0 rF1 gF59 k7 b7 d9) | apri:13(13)/13 sp0  bad_:13(18)/45 sp20  feel:17(18)/35 sp7  inha:17(17)/22 sp4  tear:14(15)/25 sp2 | drums-ret 0.84
N m=4.5 H=64   | guards FAIL (dF0 dL0 rF0 gF73 k7 b7 d9) | apri:13(13)/13 sp0  bad_:7(12)/45 sp19  feel:24(24)/35 sp7  inha:17(17)/22 sp3  tear:14(16)/25 sp3 | drums-ret 0.76
N m=6.0 H=64   | guards FAIL (dF0 dL0 rF0 gF73 k7 b7 d7) | apri:12(12)/13 sp0  bad_:6(11)/45 sp17  feel:24(25)/35 sp3  inha:17(17)/22 sp2  tear:14(15)/25 sp2 | drums-ret 0.68
N m=9.0 H=64   | guards FAIL (dF0 dL0 rF0 gF73 k7 b7 d7) | apri:11(11)/13 sp0  bad_:6(11)/45 sp16  feel:19(20)/35 sp1  inha:16(16)/22 sp1  tear:13(13)/25 sp0 | drums-ret 0.48

== RETENTION (Low-band fires per clip, baseline first) ==
```

## Round 3 — OR'd floored-novelty criterion (O) — guard-green partial
```
== SWEEP ==
guards gate: dive F=0,L=0 | riser F=0 | growl F=0 | kicks L==8 | busymix L>=7 | densemix L>=6
baseline       | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:0(0)/45 sp4  feel:4(6)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
O m=3.0 d=80 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:12(12)/13 sp0  bad_:8(10)/45 sp14  feel:16(19)/35 sp5  inha:17(18)/22 sp3  tear:12(17)/25 sp4 | drums-ret 1.00
O m=3.0 d=125 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:8(8)/13 sp0  bad_:4(6)/45 sp8  feel:5(8)/35 sp3  inha:17(18)/22 sp2  tear:10(15)/25 sp4 | drums-ret 1.00
O m=3.0 d=200 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:2(2)/45 sp4  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
O m=4.5 d=80 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:12(12)/13 sp0  bad_:7(10)/45 sp13  feel:15(18)/35 sp5  inha:17(18)/22 sp3  tear:13(18)/25 sp4 | drums-ret 1.00
O m=4.5 d=125 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:8(8)/13 sp0  bad_:3(5)/45 sp9  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:11(16)/25 sp4 | drums-ret 1.00
O m=4.5 d=200 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:2(2)/45 sp4  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
O m=6.0 d=80 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:10(10)/13 sp0  bad_:7(10)/45 sp12  feel:12(15)/35 sp4  inha:17(18)/22 sp2  tear:13(18)/25 sp4 | drums-ret 1.00
O m=6.0 d=125 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:7(7)/13 sp0  bad_:3(4)/45 sp8  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:10(15)/25 sp4 | drums-ret 1.00
O m=6.0 d=200 H=16 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:0(0)/45 sp4  feel:4(6)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
O m=3.0 d=80 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:12(12)/13 sp0  bad_:7(11)/45 sp16  feel:10(13)/35 sp7  inha:17(18)/22 sp2  tear:14(19)/25 sp4 | drums-ret 1.00
O m=3.0 d=125 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:8(8)/13 sp0  bad_:2(4)/45 sp10  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:11(16)/25 sp4 | drums-ret 1.00
O m=3.0 d=200 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:0(0)/45 sp4  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
O m=4.5 d=80 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d8) | apri:12(12)/13 sp0  bad_:7(11)/45 sp16  feel:10(13)/35 sp4  inha:17(18)/22 sp2  tear:14(19)/25 sp4 | drums-ret 1.00
O m=4.5 d=125 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:8(8)/13 sp0  bad_:2(3)/45 sp10  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:11(16)/25 sp4 | drums-ret 1.00
O m=4.5 d=200 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:0(0)/45 sp4  feel:4(6)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
O m=6.0 d=80 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:10(10)/13 sp0  bad_:4(8)/45 sp16  feel:10(13)/35 sp3  inha:17(18)/22 sp2  tear:13(18)/25 sp4 | drums-ret 1.00
O m=6.0 d=125 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d7) | apri:8(8)/13 sp0  bad_:1(2)/45 sp10  feel:5(8)/35 sp2  inha:17(18)/22 sp2  tear:11(16)/25 sp4 | drums-ret 1.00
O m=6.0 d=200 H=32 | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:0(0)/45 sp4  feel:4(6)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00

== RETENTION (Low-band fires per clip, baseline first) ==
```

## Round 4 — descending-apex sweep event v0 (K)
```
== SWEEP ==
guards gate: dive F=0,L=0 | riser F=0 | growl F=0 | kicks L==8 | busymix L>=7 | densemix L>=6
baseline       | guards PASS (dF0 dL2 rF0 gF0 k8 b8 d6) | apri:5(5)/13 sp0  bad_:0(0)/45 sp4  feel:4(6)/35 sp2  inha:17(18)/22 sp2  tear:8(13)/25 sp4 | drums-ret 1.00
K d=6 s=4      | guards FAIL (dF0 dL2 rF0 gF0 k15 b8 d7) | apri:6(7)/13 sp3  bad_:4(13)/45 sp17  feel:5(12)/35 sp34  inha:17(18)/22 sp17  tear:12(17)/25 sp19 | drums-ret 1.44
K d=6 s=6      | guards FAIL (dF2 dL2 rF0 gF0 k15 b8 d7) | apri:8(9)/13 sp3  bad_:5(12)/45 sp18  feel:5(15)/35 sp34  inha:18(19)/22 sp19  tear:12(18)/25 sp19 | drums-ret 1.48
K d=8 s=4      | guards FAIL (dF0 dL2 rF0 gF0 k15 b8 d6) | apri:6(7)/13 sp3  bad_:4(13)/45 sp17  feel:5(11)/35 sp30  inha:17(18)/22 sp17  tear:12(17)/25 sp18 | drums-ret 1.44
K d=8 s=6      | guards FAIL (dF2 dL2 rF0 gF0 k15 b8 d6) | apri:8(9)/13 sp3  bad_:5(12)/45 sp18  feel:5(14)/35 sp31  inha:17(19)/22 sp19  tear:12(18)/25 sp18 | drums-ret 1.48
K d=12 s=4     | guards FAIL (dF0 dL2 rF0 gF0 k15 b8 d6) | apri:5(5)/13 sp3  bad_:3(8)/45 sp12  feel:5(9)/35 sp21  inha:17(18)/22 sp15  tear:13(18)/25 sp17 | drums-ret 1.36
K d=12 s=6     | guards FAIL (dF2 dL2 rF0 gF0 k15 b8 d6) | apri:6(6)/13 sp3  bad_:4(7)/45 sp13  feel:5(12)/35 sp23  inha:17(19)/22 sp17  tear:13(19)/25 sp17 | drums-ret 1.44

== RETENTION (Low-band fires per clip, baseline first) ==
```
