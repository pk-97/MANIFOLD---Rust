# Fluid Sim 2D

*A swarm of particles that follow their own trails, organizing into shifting filament networks.*

## How it works

Fluid Sim 2D takes no input. It builds its own image from a swarm of moving particles, starting from a plain scatter and organizing it into structure over time.

The whole simulation is built from named node groups in a graph editor. Each group is one step of the work, and this report walks through them in the order they run.

### The thing that persists

Only one thing survives from one frame to the next. Everything else is rebuilt each frame from it:

- the **particles**, a large set of moving points, each holding a position on the canvas.

The particles are the entire memory of the simulation. Each frame reads them, works out where they should move, and writes their new positions back.

### The loop in one breath

Each frame runs this pipeline. The key to following it is knowing where each step gets its input:

1. **Render Density.** Read the particles and stamp them into a density image, which records how many particles are stacked at each pixel.
2. **Flow Field.** Read the density and turn it into a push direction at every point on the canvas.
3. **Move Particles.** Read the particles and step each one along the push direction under it, then save the result.
4. **Display.** Shade the density into the final frame.

Two more groups set things up. **Spawn Particles** creates the initial scattered particles, and the control groups, Resolution Scaling and Clip Triggers, supply the numbers and the live performance triggers.

The loop closes between Move Particles and Render Density. The particles that Move writes become the particles that Render reads on the next frame. Because the push directions come from the density, and the density comes from the particles, the particles end up following their own trails, which is why they pull together into filaments instead of staying a flat cloud.

The pipeline is easiest to follow from the first frame, where the particles are a plain scatter, watching the network organize from there.

### Reading the images

[[preview-legend]]

### Setup: particles and controls

These three groups produce no image. They prepare the particles and the numbers the pipeline runs on.

**Spawn Particles** *(Reads: particle count and canvas size. Produces: the initial particle positions.)* scatters the starting particles across the canvas. After the first frame the simulation carries them forward itself, so this is the initial condition rather than a per-frame step.

**Resolution Scaling** *(Reads: canvas width and height. Produces: particle count, blur radii, scatter strength, display intensity and zoom.)* derives every size-dependent number from the output resolution, so the look stays consistent whether the canvas is 512 pixels or 4K.

**Clip Triggers** *(Reads: the clip trigger count. Produces: reset, turbulence, injection points, and the flow slope and rotation.)* turns clip launches into momentary behaviors. This is the performance surface. Launching a clip can reset the swarm, inject particles at a point, or push turbulence into the motion.

### Phase 1: Render Density

*Reads: the particles. Produces: a density image, and a blurred copy of it.*

Render Density stamps every particle into an image, adding up where particles land on the same pixel. The result is the **density**: an image whose brightness at each point is how many particles are piled up there. A blurred copy is produced alongside it, which the next two steps use to read the broad shape of the swarm rather than reacting to single particles.

![Density stamped from the particles](preview://Render Density)
*The bright filaments are where particles have gathered. The density is mostly dark because the particles concentrate into thin lines.*

### Phase 2: Flow Field

*Reads: the blurred density. Produces: a push direction at every pixel, for Move Particles.*

Flow Field turns the density into motion. It computes the gradient of the density: at each pixel, the direction in which the density rises fastest, which points toward where particles are already gathered. It then rotates that direction by an angle, set by the Curl control, so the particles do not simply collapse onto each other but spiral and run along the trails. The output is a push direction for every point on the canvas, shown here as color.

![Flow Field](preview://Flow Field)

### Phase 3: Move Particles

*Reads: the particles, the push directions (Phase 2), and the trigger behaviors (Clip Triggers). Produces: the moved particles, saved as next frame's input to Phase 1.*

Move Particles produces no image of its own, because it works on the particle positions, which you see through the density in Phase 1. It steps each particle along the push direction beneath it, adds a little turbulence so the motion is not perfectly smooth, applies any injection or reset from Clip Triggers, and keeps particles from piling into a single blob (Anti-Clump). The new positions it writes are the particles the next frame reads.

### Phase 4: Display

*Reads: the density (Phase 1). Produces: the on-screen frame. Does not feed back into the simulation.*

Display shades the density for the screen, scaling its brightness with Contrast and its framing with Zoom. It only reads the density, so nothing here affects the next frame.

![Display](preview://Display)

### Putting it together: the first few frames

- **Frame 1.** The particles are a plain scatter from Spawn. Render Density shows a flat, even speckle of dots. There is no structure yet, so the push directions are weak and the particles barely move.
- **Early frames.** Small clumps form by chance. Wherever particles gather, the density is higher, the push points toward it more strongly, and more particles are drawn in. The clumps reinforce themselves.
- **Later frames.** The reinforcing clumps stretch into lines, and the lines connect into a web, because particles moving toward density also run along it. The network keeps drifting and rewiring rather than freezing.

![Output](preview://Output)
*The final composited image after warm-up.*

## Controls

### Simulation

| Control | What it does |
|---|---|
| Speed | How fast each particle steps along the push direction every frame. |
| Particle Count | Number of particles, in millions. More particles give denser, finer networks. |
| Force | Strength of the force injected at trigger points. |
| Flow | How strongly, and in which direction, the density pushes the particles. Negative values push away from dense areas. |
| Curl | How far the push is rotated away from straight-toward-density, which sets how much the particles spiral and run along trails instead of collapsing onto them. |
| Turbulence | Random jitter added to particle motion, breaking up smooth flow. |
| Anti-Clump | Repulsion that stops particles from piling into a single dense blob. |
| Feather | Softness of the density. Higher blurs the swarm so the flow reads broad shapes, not single particles. |

### Look

| Control | What it does |
|---|---|
| Contrast | Contrast of the final density on screen. |
| Scale | Zoom of the simulation space. |
| Fill | Scales how many of the particles are active. |
| Clip Trigger | Fires the selected trigger behavior on a clip launch. |
| Clip Trigger Mode | Which behavior a trigger fires: reset, injection, turbulence, and so on. |

## Technical notes

For readers who want the specifics behind the plain-language descriptions above.

- **Particles.** A position array carried frame to frame by an array feedback node. Spawn seeds it once. Move Particles integrates it each frame and writes it back.
- **Render Density.** Particles are scatter-accumulated into a single-channel density target, then separably blurred (radii scaled to canvas size by Resolution Scaling) to give the smooth density the force reads.
- **Flow Field.** Central-difference gradient of the blurred density, rotated by the Curl angle and scaled by Flow, giving a per-pixel force that is largely tangent to the density contours. This trail-following is the same mechanism as Physarum agent models.
- **Move Particles.** Euler integration of each particle along the sampled force, plus turbulence, anti-clump repulsion, and gated injection or reset from Clip Triggers.
- **Display.** Tone mapping of the density (Contrast) with a UV zoom (Scale), then the final output.
- **Control groups.** Resolution Scaling derives particle count, blur radii, scatter strength, intensity, and zoom from the canvas area. Clip Triggers gates an envelope into reset, turbulence, injection point and amount, and flow slope and rotation, driven by the clip trigger count and mode.
