# Oily Fluid

*A self-feeding fluid simulation that marbles its own image into slow, drifting, oil-on-water patterns.*

## How it works

Oily Fluid takes no input. It generates its own picture, starting from a black frame and building the image over time.

The whole simulation is built from named node groups in a graph editor. Each group is one step of the work, and this report walks through them in the order they run.

### The two things that persist

Only two things survive from one frame to the next. Everything else is rebuilt each frame from these two:

- the **color**, the visible image, and
- the **flow**, a direction and speed stored at every pixel, describing which way that part of the image is moving and how fast.

Both start black: no image, no motion.

### The loop in one breath

Each frame runs four phases in order. The key to following them is knowing where each phase gets its input:

1. **Make the seed.** Generate a faint new noise pattern. Reads nothing, it is self-generating.
2. **Build the flow.** Read last frame's color and turn its edges into motion.
3. **Stir the color.** Read last frame's color, push it along this frame's flow, and mix in the seed.
4. **Show it.** Shade the new color for display only.

The loop closes between Phase 3 and Phase 2. The color that Phase 3 produces becomes the color that Phase 2 reads on the next frame. That chain runs all the way back to the initial black frame, which is why a fresh start has nothing to work with and the image has to accumulate.

The phases are easiest to follow from the very first frame, where color and flow are both black, watching the image build from there.

### Reading the images

[[preview-legend]]

### Phase 1: Make the seed

*Reads: nothing. Produces: a faint noise layer for Phase 3.*

Inject Noise produces a smooth, organic pattern (simplex noise, a gradient noise related to Perlin noise rather than uncorrelated static). It drifts slowly because the node samples a moving plane through a 3D noise volume. The pattern is then scaled down hard by the Noise parameter (roughly 0.01), so only a very small amount is passed on each frame.

On the first frame, with color and flow both black, this faint pattern is the only signal in the whole system. It is the ultimate source of every bit of image content, and Phase 3 is where it gets added in.

![Noise pattern at full strength, before scaling](preview://node:noise_combine)

*Shown at full strength. In the graph it is scaled to roughly 0.01 before use. The final image is built from many of these tiny contributions accumulated over time, not from one strong burst.*

### Phase 2: Build the flow

*Reads: last frame's color, and last frame's flow. Produces: this frame's flow for Phase 3.*

This phase turns the picture into motion. It runs in three steps.

**Curl Forcing** looks at the color and finds its gradient: at each pixel, the direction in which the color changes fastest. That direction points across the boundary between a darker and a lighter region. Rotating it by 90 degrees turns across into along, so the rotated direction runs parallel to the boundary, following the contour of the region. A force aimed along a region's contour pushes fluid around the region rather than out through its edge, and that circulation is the curl the phase is named for. On a black frame there is no gradient, so there is no force. Once the color has structure, its contours decide where the fluid turns.

![Curl Forcing](preview://Curl Forcing)

**Smooth Velocity** drops the flow to a lower resolution and blurs it (a separable Gaussian blur). A sharp, detailed flow would shred the color into grain. A smooth, broad flow carries it in coherent sheets, which is what gives the result its thick, viscous look.

![Smooth Velocity](preview://Smooth Velocity)

**Advect Velocity** assembles this frame's flow from three inputs. It keeps last frame's flow, carrying its momentum forward, reduces it slightly so motion cannot build up without limit (set by Velocity Damp), and adds the new swirls from Curl Forcing.

![Advect Velocity](preview://Advect Velocity)

### Phase 3: Stir the color

*Reads: last frame's color, this frame's flow (Phase 2), and the seed (Phase 1). Produces: the new color, sent to Phase 4 and saved as next frame's input to Phase 2.*

Advect Color updates the visible image in three moves:

1. It moves the current color along the flow. This is advection: each pixel takes its color from the point just upstream that the flow would have carried to it, so the picture is dragged along the current, like ink on water. (The same operation moved the flow's own motion forward in Phase 2, which is where the flow's momentum comes from.)
2. It fades the result very slightly. The fade keeps almost all of the existing color, controlled by Feedback at roughly 0.9999.
3. It mixes in the scaled seed from Phase 1.

Because so little is lost per frame, every new seed layer is laid on top of everything kept so far. On frame 1 there is nothing to advect, so the output is a single faint layer of noise. Over many frames the kept layers pile up while advection transports and folds them, and that folding is what produces the marbling.

![Advect Color](preview://Advect Color)

This new color is the image. It goes to Phase 4 for display, and it is written back as the color that Phase 2 reads next frame.

### Phase 4: Show it

*Reads: the new color (Phase 3). Produces: the on-screen frame. Does not feed back into the simulation.*

This phase only shades the image for display. Nothing here affects the next frame.

Render Modes treats the color's brightness as a surface, where bright areas are high ground and dark areas are low. From that surface it builds a normal map, an image that records which way the surface faces at each point, which is what lighting needs in order to shade it. Lighting that surface is what gives the image its raised, wet look. The Mode control picks the shading style (Oil Slick, Flow Field, Lines, Height Map, or PBR). The techniques behind each are described in the technical notes below.

![Render Modes](preview://Render Modes)

Color Grade then applies the final brightness, hue, contrast, and saturation, producing the frame on screen.

![Color Grade](preview://Color Grade)

### Putting it together: the first few frames

- **Frame 1.** Color and flow are black. Phase 1 lays down a faint noise layer. Phases 2 and 3 have no structure to act on yet, so the output is nearly black.
- **Frame 2.** That faint layer is now structure. Curl Forcing derives a small flow from its edges, and Phase 3 advects the layer along that flow and adds another seed.
- **Frame 3 onward.** Because each frame keeps almost all of the previous color, the seeds accumulate, advection folds them into marbling, and overall brightness rises.

The flow settles within a second or two. The color and surface relief keep building over the next several seconds. That is why a fresh start looks flat before the marbling appears.

![Output](preview://Output)

*The final composited image after warm-up.*

## Controls

### Simulation

| Control | What it does |
|---|---|
| Speed | Overall flow rate. Scales how fast flow and color move each frame. |
| Feedback | How long color is retained before fading. Higher values give longer trails and denser buildup. |
| Curl | Swirl strength. How strongly edges rotate the flow into spirals. |
| Noise | How much new texture is injected each frame. |
| Velocity Damp | How quickly the flow loses energy. Lower values settle it sooner. |
| Velocity Displace | How far the flow advects itself each step. |
| Color Displace | How far the color is advected along the flow each step. |

### Look

| Control | What it does |
|---|---|
| Mode | Final look: Oil Slick, Flow Field, Height Map, PBR, or Lines. |
| Relief | Strength of the lit surface relief. |
| Chroma | Channel-offset amount in Oil Slick mode. |
| Brightness, Saturation, Hue, Contrast | Final color grade on the output. |

## Technical notes

For readers who want the specifics behind the plain-language descriptions above.

- **Seed.** Simplex noise sampled on a translating plane through a 3D volume. Output scaled by Noise (around 0.01) per frame.
- **Curl Forcing.** Gradient of the color, rotated 90 degrees to give a divergence-free forcing term along region contours.
- **Smooth Velocity.** Downsample followed by a separable Gaussian blur to keep transport coherent.
- **Advect Velocity and Advect Color.** Semi-Lagrangian advection by upstream sampling. Velocity retains momentum minus Velocity Damp. Color retains Feedback (around 0.9999) per frame before the seed is added.
- **Render modes.**
  - *Oil Slick.* Per-channel offset along the flow direction for a petrol-like chromatic shift.
  - *Flow Field and Lines.* Line integral convolution (LIC), smearing a texture along the flow to make it visible.
  - *Height Map.* Lambertian diffuse lighting of the brightness-derived normal map.
  - *PBR.* Matcap base with a Fresnel rim and a Blinn specular highlight.
