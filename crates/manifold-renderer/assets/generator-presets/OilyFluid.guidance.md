# Oily Fluid

*A self-feeding fluid simulation that marbles the image into slow, drifting, oil-on-water patterns.*

## How it works

Oily Fluid builds its own picture from nothing. There is no input clip — it starts on a black screen and grows the image itself, a little more every frame.

To do that it keeps two things and updates them each frame:

- the **color** — the picture you actually see, and
- the **flow** — a hidden map of which way each part of the picture is being pushed.

Before the first frame, both are black: no picture, no motion. Every frame then runs the same four steps. The clearest way to read them is to start from that black screen and watch the picture appear.

### Phase 1 — Make the seed

Inject Noise lays down a soft, cloudy pattern (simplex noise, the smooth Perlin kind, not TV static), drifting slowly because it is a moving slice through a 3D noise field. It then scales this way down, to the Noise amount of about 0.01, so what actually gets handed on is a faint whisper.

On the very first frame, when everything is black, this whisper is the only thing on the screen. It is the raw material the whole image is built from, and Phase 3 is where it gets added in.

![The noise seed at full amplitude, before it is scaled for injection](preview://node:noise_combine)

*Shown at full amplitude. In the graph it is scaled down to a whisper before use. The image is built from many of those whispers stacked over time, not one bright noise field.*

### Phase 2 — Build the flow

Now the simulation works out how to push the color around. It looks at **the color as it currently stands** — on the first frame that is still almost black, on every later frame it is the picture built so far — and turns it into a flow.

- **Curl Forcing** finds the edges in that color, the lines where one shade meets another, and turns each into a sideways push, rotated a quarter turn so the liquid circles those edges instead of spreading off them (this rotation is the curl). On a black screen there are no edges, so nothing is pushed yet; once the picture has shape, its own edges become the swirls.

![Curl Forcing](preview://Curl Forcing)

- **Smooth Velocity** takes the flow the simulation already had going and softens it, shrinking then blurring it (a separable Gaussian blur), so the motion happens in broad slow sheets rather than sharp jitter. That softness is where the thick, oily feel comes from.

![Smooth Velocity](preview://Smooth Velocity)

- **Advect Velocity** rolls these into the flow for this frame: the existing motion carries itself forward (self-advection — it has momentum, like real water), loses a little energy so it cannot run away (damping), and picks up the new swirls from Curl Forcing.

![Advect Velocity](preview://Advect Velocity)

### Phase 3 — Stir the color

This is where the picture actually changes. Advect Color takes **the color as it stands**, drags every pixel of it along the **flow just built in Phase 2** (advection, the core transport step), fades it a touch, and mixes in the **whisper of seed from Phase 1**.

On the first frame there is nothing to drag — the color is black — so all you get is that one faint layer of noise. But the fade keeps almost all of the existing color (set by Feedback, near 0.9999), so on every following frame the new whisper lands on top of everything kept so far. Drag, fade, add, repeated, is what turns a stack of whispers into flowing marble.

![Advect Color](preview://Advect Color)

The color this phase produces is the new picture. It is what Phase 4 shows, and it is also what the *next* frame reads back in Phase 2 to decide the next flow.

### Phase 4 — Show it

Finally the color is lit. Render Modes reads the color's brightness as a surface height, works out which way that surface tilts (a normal map), and shines light across it for a raised, wet look. The Mode control picks the style:

- **Oil Slick** splits colors along the flow for a petrol sheen (flow-driven chromatic shift).
- **Flow Field** and **Lines** smear a texture along the current to draw it (line integral convolution, LIC).
- **Height Map** lights the relief plainly (Lambertian diffuse).
- **PBR** makes it glossy (matcap base, Fresnel rim, Blinn specular).

![Render Modes](preview://Render Modes)

Color Grade then applies the final brightness, hue, and contrast, and that is the frame you see.

![Color Grade](preview://Color Grade)

### The picture, frame by frame

Frame 1 starts black, so Phase 1 lays a whisper of noise and Phases 2 and 3 have almost nothing to work with — you end with a barely-there cloud. Frame 2 now has that cloud to read, so Phase 2 turns its edges into a small flow and Phase 3 smears the cloud along it and adds another whisper. Because each frame keeps almost all of the last, the layers pile up, the swirls fold them into marbling, and the brightness climbs. A few seconds of that and you have the full image. So "the color as it stands" that Phase 2 reads is simply whatever Phase 3 produced the frame before, and the chain traces all the way back to the first black screen.

![Output](preview://Output)

*The final composited image after warm-up. The flow develops within a second or two, while the color and surface relief build over the next several seconds, so a fresh start looks flat before the marbling deepens.*

## Controls

### Simulation

| Control | What it does |
|---|---|
| Speed | Overall flow rate. Scales how fast the flow and color move each frame. |
| Feedback | How long color lingers before fading. Higher gives longer trails and denser buildup. |
| Curl | Swirl strength. How hard the edges twist the flow into spirals. |
| Noise | How much fresh texture is injected each frame. |
| Velocity Damp | How quickly the flow loses energy. Lower settles it sooner. |
| Velocity Displace | How far the flow drags itself each step. |
| Color Displace | How far the color is dragged along the flow each step. |

### Look

| Control | What it does |
|---|---|
| Mode | Final look: Oil Slick, Flow Field, Height Map, PBR, or Lines. |
| Relief | Strength of the lit surface relief. |
| Chroma | Color-split amount in Oil Slick mode. |
| Brightness, Saturation, Hue, Contrast | Final color grade on the output. |
