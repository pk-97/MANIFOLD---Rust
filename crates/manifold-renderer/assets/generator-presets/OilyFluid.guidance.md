# Oily Fluid

*A self-feeding fluid simulation that marbles the image into slow, drifting, oil-on-water patterns.*

## How it works

Oily Fluid is a generator: it starts from black and makes its own image with no input. It keeps two pictures in memory and carries them from one frame to the next:

- the **color** — what you see, and
- the **flow** — an invisible field of arrows, one per pixel, saying which way that pixel is moving.

Both start black. Each frame reads those two, builds new versions, and writes them back, so every frame is a small edit of the one before. The engine is that the two feed each other: the **color's shape decides where the flow swirls**, and the **flow then drags the color around**. Add a trickle of fresh noise so it never runs dry, and that two-way loop is the whole thing.

A frame runs in four phases. What matters is what each one reads and what it hands to the next.

### Phase 1: Make the seed

Inject Noise generates a soft, cloud-like texture (simplex noise, a smooth gradient noise like Perlin, not harsh white-noise static). It is a moving slice through a 3D noise field, so it drifts and never quite repeats. This is the only new content the simulation ever gets. On a black start it is the whole image, and it keeps trickling in so the patterns never die out.

![The noise seed at full amplitude, before it is scaled for injection](preview://node:noise_combine)

*Shown at full amplitude. In the graph this pattern is scaled down to the Noise value (around 0.01) before being added to the color each frame. That looks like almost nothing on its own, but because Feedback keeps about 99.99% of the color, the whisper accumulates over many frames into the full image.*

**Hands to:** nothing yet. The scaled seed is set aside and only enters the picture at the very end of Phase 3.

### Phase 2: Build the flow

Phase 2 reads the **color from the previous frame** and turns it into the flow that will move the color this frame. This is the first half of the loop: the picture decides the motion.

- **Curl Forcing** reads that stored color and finds where it changes fastest, its edges (the gradient, by central differences), giving an arrow across each edge. It normalizes those to pure direction and rotates them 90°. A gradient turned 90° becomes a swirl (curl, a flow's rotation or vorticity), so the boundaries in the picture become the places the liquid circles.

![Curl Forcing](preview://Curl Forcing)

- **Smooth Velocity** reads the **stored flow** and softens it, shrinking it (box-filter downsample) then blurring it (separable Gaussian). A sharp flow tears the color into grain; a soft one moves it in slow thick sheets, the oily heaviness.

![Smooth Velocity](preview://Smooth Velocity)

- **Advect Velocity** combines them into the new flow: the smoothed flow drags itself forward along the stored flow (self-advection), eases off so it cannot blow up (damping), and adds in the swirls from Curl Forcing.

![Advect Velocity](preview://Advect Velocity)

**Hands to:** the new flow is written back as next frame's flow, and passed straight into Phase 3 to move the color now.

### Phase 3: Stir the color

With the new flow in hand, Phase 3 moves the color. Advect Color takes three inputs and combines them in order:

1. the **stored color** from the previous frame,
2. the **new flow from Phase 2**, used to drag every pixel of that color along the current (advection, the core semi-Lagrangian transport step),
3. the **seed from Phase 1**, mixed in after a slight fade (decay, set by Feedback) that lets old color die off.

This is the second half of the loop: the motion moves the picture. Drag, fade, refill, every frame — that is the marbling.

![Advect Color](preview://Advect Color)

**Hands to:** this becomes the new color, written back to memory. It does three jobs at once. Phase 4 displays it. The next frame stores it. And next frame's Phase 2 reads it to build the next flow — which is what closes the loop: the color you just made shapes the motion that will move it.

### Phase 4: Show it

Phase 4 only reads the color and lights it; nothing here feeds back into the simulation. Render Modes reads the color's brightness as a height, finds the surface tilt (normals, a normal map), and shines light across it for wet-looking relief. The Mode control picks the style:

- **Oil Slick** splits colors along the flow (flow-driven chromatic shift) for the petrol sheen.
- **Flow Field** and **Lines** smear a texture along the current (line integral convolution, LIC).
- **Height Map** lights the relief plainly (Lambertian diffuse).
- **PBR** makes it glossy (matcap base, Fresnel rim, Blinn specular).

![Render Modes](preview://Render Modes)

Color Grade then applies the final brightness, hue, and contrast.

![Color Grade](preview://Color Grade)

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

## Result

![Output](preview://Output)

*The final composited image after warm-up. The flow field develops within a second or two, while the color amplitude and surface relief build over the following seconds, so a fresh start looks flat before the marbling deepens.*
