#include "coupling_viscosity_probe.h"
#include "flip_engine/macvelocityfield.h"
#include "flip_engine/meshlevelset.h"
#include "flip_engine/particlelevelset.h"
#include "flip_engine/viscositysolver.h"
#include "flip_engine/viscousboundaryreaction.h"
#include "flip_engine/threadutils.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstring>
#include <limits>
#include <stdexcept>

namespace {
using Point = std::array<double, 3>;
constexpr double DX = 0.25;
constexpr int N = 12;
constexpr double DT = 1.0 / 60.0;
const Point center = {1.37, 1.44, 1.51};

void require(bool value, const char *message) {
    if (!value) { throw std::runtime_error(message); }
}

template<class Function> void rejects(Function function, const char *message) {
    bool rejected = false;
    try { function(); } catch (const std::exception &) { rejected = true; }
    require(rejected, message);
}

Array3d<float> &component(MACVelocityField &field, int axis) {
    return *(axis == 0 ? field.getArray3dU() : axis == 1 ? field.getArray3dV() : field.getArray3dW());
}

template<class Function> void each_face(Function function) {
    for (int axis = 0; axis < 3; ++axis) {
        for (int k = 0; k < N + (axis == 2); ++k) {
            for (int j = 0; j < N + (axis == 1); ++j) {
                for (int i = 0; i < N + (axis == 0); ++i) {
                    Point position = {(i+0.5)*DX, (j+0.5)*DX, (k+0.5)*DX};
                    position[axis] -= DX/2;
                    function(axis, GridIndex(i,j,k), position);
                }
            }
        }
    }
}

Point cross(const Point &a, const Point &b) {
    return {a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0]};
}

Point rigid_velocity(const Point &p) {
    const Point r = {p[0]-center[0], p[1]-center[1], p[2]-center[2]};
    Point v = cross({0.17,-0.23,0.31},r);
    v[0] += 0.2; v[1] -= 0.1; v[2] += 0.05;
    return v;
}

struct Scene {
    MeshLevelSet solid{N,N,N,DX};
    ParticleLevelSet liquid{N,N,N,DX};
    Array3d<float> viscosity{N,N,N,0.0f};
    MACVelocityField velocity{N,N,N,DX};

    Scene(double nu, bool rigid, bool moving, bool variable) {
        // Fully wet extension into a closed tank and its internal obstacle.
        // Every solved face lies in the unit-volume interior of the native
        // liquid volume grid. This makes its mass independently rho * dx^3.
        liquid.getPhiGrid()->fill(-1.0f);
        const double lo = 1.25*DX, hi = (N-1.25)*DX;
        for (int k=0;k<=N;++k) for (int j=0;j<=N;++j) for (int i=0;i<=N;++i) {
            const Point p = {i*DX,j*DX,k*DX};
            double tank = std::min({p[0]-lo,hi-p[0],p[1]-lo,hi-p[1],p[2]-lo,hi-p[2]});
            const double obstacle = std::max({std::abs(p[0]-1.47)-0.38,
                std::abs(p[1]-1.56)-0.44,std::abs(p[2]-1.40)-0.32});
            solid.set(i,j,k,std::min(tank,obstacle));
        }
        for (int k=0;k<N;++k) for (int j=0;j<N;++j) for (int i=0;i<N;++i) {
            const double factor = variable ? 0.5 + 0.02*i + 0.03*j + 0.01*k : 1.0;
            viscosity.set(i,j,k,nu*factor);
        }
        each_face([&](int axis, GridIndex g, const Point &p) {
            double value = 0.0;
            if (rigid) {
                value = rigid_velocity(p)[axis];
            } else if (is_fluid(axis,g)) {
                value = 0.3*std::sin(0.61*g.i + 0.37*g.j + 0.23*g.k + axis)
                    + 0.2*std::cos(0.17*g.i - 0.43*g.j + 0.31*g.k + 0.4*axis);
            } else if (moving) {
                value = rigid_velocity(p)[axis];
            }
            component(velocity,axis).set(g,value);
        });
    }

    bool is_fluid(int axis, GridIndex face) {
        const int normal = axis == 0 ? face.i : axis == 1 ? face.j : face.k;
        if (normal == 0 || normal == N) { return false; }
        GridIndex left = face;
        if (axis == 0) { --left.i; }
        if (axis == 1) { --left.j; }
        if (axis == 2) { --left.k; }
        return solid.getDistanceAtCellCenter(left) + solid.getDistanceAtCellCenter(face) > 0.0f;
    }

    bool solved(int axis, GridIndex face) {
        return face.i > 0 && face.j > 0 && face.k > 0
            && face.i < N && face.j < N && face.k < N && is_fluid(axis,face);
    }

    ViscositySolverParameters params(double dt, double rho, ViscousBoundaryReaction *reaction) {
        ViscositySolverParameters p;
        p.cellwidth=DX; p.deltaTime=dt; p.velocityField=&velocity;
        p.liquidSDF=&liquid; p.solidSDF=&solid; p.viscosity=&viscosity;
        p.errorTolerance=1e-9; p.maxIterations=900;
        p.boundaryReaction=reaction; p.reactionDensity=rho;
        return p;
    }
};

double difference(MACVelocityField &a, MACVelocityField &b) {
    double error=0.0;
    each_face([&](int axis, GridIndex g, const Point &) {
        error=std::max(error,std::abs(double(component(a,axis).get(g))-component(b,axis).get(g)));
    });
    return error;
}

bool same_bits(MACVelocityField &a, MACVelocityField &b) {
    for (int axis=0;axis<3;++axis) {
        auto &left=component(a,axis), &right=component(b,axis);
        const size_t count=static_cast<size_t>(left.width)*left.height*left.depth;
        if (std::memcmp(left.getRawArray(),right.getRawArray(),count*sizeof(float))!=0) { return false; }
    }
    return true;
}

struct Measurement {
    Point liquid_linear{}, liquid_angular{}, solid_linear{}, solid_angular{};
    Point boundary_linear_scale{}, boundary_angular_scale{};
    double before_energy=0.0, after_energy=0.0;
    double rigid_error=0.0, max_impulse=0.0;
};

Measurement measure(Scene &scene, MACVelocityField &before, ViscousBoundaryReaction &reaction,
                    double density) {
    Measurement m;
    const double mass=density*DX*DX*DX;
    each_face([&](int axis, GridIndex g, const Point &position) {
        const double impulse=reaction.impulse(axis,g);
        require(std::isfinite(impulse),"nonfinite viscous impulse");
        Point p = {position[0]-center[0],position[1]-center[1],position[2]-center[2]};
        Point vector{};
        vector[axis]=impulse;
        Point moment=cross(p,vector);
        m.max_impulse=std::max(m.max_impulse,std::abs(impulse));
        for (int a=0;a<3;++a) {
            m.solid_linear[a]+=vector[a]; m.solid_angular[a]+=moment[a];
            m.boundary_linear_scale[a]+=std::abs(vector[a]);
            m.boundary_angular_scale[a]+=std::abs(moment[a]);
        }
        if (!scene.solved(axis,g)) { return; }
        require(impulse == 0.0,"viscous reaction was written onto a fluid unknown");
        const double initial=component(before,axis).get(g);
        const double final=component(scene.velocity,axis).get(g);
        require(std::isfinite(initial) && std::isfinite(final),"nonfinite viscosity output");
        m.before_energy+=0.5*mass*initial*initial;
        m.after_energy+=0.5*mass*final*final;
        m.rigid_error=std::max(m.rigid_error,std::abs(final-initial));
        vector[axis]=mass*(final-initial);
        moment=cross(p,vector);
        for (int a=0;a<3;++a) { m.liquid_linear[a]+=vector[a]; m.liquid_angular[a]+=moment[a]; }
    });
    return m;
}

void include_balance(const Measurement &m, ManifoldViscousBoundaryProbe &result) {
    for (int a=0;a<3;++a) {
        const double linear_absolute=std::abs(m.liquid_linear[a]+m.solid_linear[a]);
        const double angular_absolute=std::abs(m.liquid_angular[a]+m.solid_angular[a]);
        result.max_linear_absolute_error=std::max(result.max_linear_absolute_error,linear_absolute);
        result.max_angular_absolute_error=std::max(result.max_angular_absolute_error,angular_absolute);
        // Opposing wall stresses can have a nearly zero net impulse. Normalize
        // by the total magnitude exchanged with those walls, so cancellation
        // cannot magnify native f32 rounding into a spurious relative failure.
        const double linear=linear_absolute/std::max(1.0,m.boundary_linear_scale[a]);
        const double angular=angular_absolute/std::max(1.0,m.boundary_angular_scale[a]);
        result.max_linear_balance_error=std::max(result.max_linear_balance_error,linear);
        result.max_angular_balance_error=std::max(result.max_angular_balance_error,angular);
    }
    ++result.cases;
}
} // namespace

void run_viscous_boundary_probe(ManifoldViscousBoundaryProbe &result) {
    struct ThreadLimit {
        int previous=ThreadUtils::getMaxThreadCount();
        ThreadLimit() { ThreadUtils::setMaxThreadCount(2); }
        ~ThreadLimit() { ThreadUtils::setMaxThreadCount(previous); }
    } threads;
    result={};
    ViscousBoundaryReaction reaction;
    require(reaction.prepare(N,N,N,DX),"reaction storage preparation failed");
    for (bool moving : {false,true}) for (bool variable : {false,true}) {
        for (double nu : {0.01,1.0}) for (double dt : {DT,DT/2}) {
            Scene scene(nu,false,moving,variable);
            MACVelocityField before=scene.velocity;
            ViscositySolver solver;
            require(solver.applyViscosityToVelocityField(scene.params(dt,1000,&reaction)),
                    "native viscosity solve failed");
            require(reaction.hasSolution(),"successful solve has no viscous reaction");
            const Measurement m=measure(scene,before,reaction,1000);
            include_balance(m,result);
            if (!moving) {
                const double ratio=m.after_energy/m.before_energy;
                result.max_passive_energy_ratio=std::max(result.max_passive_energy_ratio,ratio);
                if (!variable && dt == DT) {
                    if (nu == 0.01) { result.low_viscosity_energy_ratio=ratio; }
                    else { result.high_viscosity_energy_ratio=ratio; }
                }
            }
        }
    }
    Scene rigid(1.0,true,false,true);
    MACVelocityField rigid_before=rigid.velocity;
    ViscositySolver rigid_solver;
    require(rigid_solver.applyViscosityToVelocityField(rigid.params(DT,1000,&reaction)),
            "rigid-motion viscosity solve failed");
    const Measurement rigid_measure=measure(rigid,rigid_before,reaction,1000);
    result.max_rigid_velocity_error=rigid_measure.rigid_error;
    result.max_rigid_impulse=rigid_measure.max_impulse;
    include_balance(rigid_measure,result);

    Scene first(0.7,false,true,true), second(0.7,false,true,true), baseline(0.7,false,true,true);
    ViscousBoundaryReaction half;
    require(half.prepare(N,N,N,DX),"half-density reaction preparation failed");
    ViscositySolver a,b,c;
    require(a.applyViscosityToVelocityField(first.params(DT,1000,&reaction))
        && b.applyViscosityToVelocityField(second.params(DT,500,&half))
        && c.applyViscosityToVelocityField(baseline.params(DT,1000,nullptr)),"density/native baseline failed");
    require(difference(first.velocity,second.velocity)<1e-7 && difference(first.velocity,baseline.velocity)<1e-7,
            "reaction measurement changed the native velocity solve");
    each_face([&](int axis,GridIndex g,const Point &) {
        const double expected=2*half.impulse(axis,g), actual=reaction.impulse(axis,g);
        result.max_density_scaling_error=std::max(result.max_density_scaling_error,
            std::abs(expected-actual)/std::max(1.0,std::abs(actual)));
    });

    Scene invalid(0.7,false,true,true);
    MACVelocityField unchanged=invalid.velocity;
    auto params=invalid.params(DT,-1,&reaction);
    require(!a.applyViscosityToVelocityField(params),"negative reaction density accepted");
    require(!reaction.hasSolution() && difference(invalid.velocity,unchanged)==0,
            "invalid reaction request mutated fluid or retained old output");
    rejects([&] { reaction.impulse(0,GridIndex(1,1,1)); },"stale viscous reaction remained readable");
    // The pinned solver accepts an exhausted iteration budget when its absolute
    // residual is below 10. Use a larger RHS to exercise its actual failure path.
    each_face([&](int axis,GridIndex g,const Point &) {
        component(invalid.velocity,axis).set(g,100*component(invalid.velocity,axis).get(g));
    });
    unchanged=invalid.velocity;
    params=invalid.params(DT,1000,&reaction);
    params.maxIterations=0;
    require(!a.applyViscosityToVelocityField(params),"forced viscosity failure was accepted");
    require(!reaction.hasSolution() && difference(invalid.velocity,unchanged)==0,
            "failed viscosity solve mutated fluid or retained a reaction");
    params=invalid.params(DT,std::numeric_limits<double>::max(),&reaction);
    require(!a.applyViscosityToVelocityField(params),"viscous impulse overflow accepted");
    require(!reaction.hasSolution() && difference(invalid.velocity,unchanged)==0,
            "viscous extraction overflow partially committed output");
    require(a.applyViscosityToVelocityField(invalid.params(DT,1000,&reaction)),
            "reaction storage was not reusable after failure");

    Scene nonfinite(0.0,false,false,false);
    nonfinite.velocity.setU(5,5,5,std::numeric_limits<float>::quiet_NaN());
    MACVelocityField nonfinite_before=nonfinite.velocity;
    require(!a.applyViscosityToVelocityField(nonfinite.params(DT,1000,&reaction))
            && !reaction.hasSolution() && same_bits(nonfinite.velocity,nonfinite_before),
            "zero viscosity hid nonfinite velocity or modified rejected input");

    Scene dry(0.7,false,false,false);
    dry.liquid.getPhiGrid()->fill(1.0f);
    ViscositySolver dry_solver;
    require(dry_solver.applyViscosityToVelocityField(dry.params(DT,1000,&reaction))
            && reaction.hasSolution(),"empty liquid did not accept a zero reaction");
    each_face([&](int axis,GridIndex g,const Point &) {
        require(reaction.impulse(axis,g)==0.0,"empty liquid retained a viscous reaction");
    });
    rejects([&] { reaction.impulse(3,GridIndex(0,0,0)); },"invalid reaction axis remained readable");
    rejects([&] { reaction.impulse(0,GridIndex(-1,0,0)); },"invalid reaction face remained readable");
    for (int failure=0;failure<3;++failure) {
        require(reaction.beginCapture(N,N,N,DX,1000),"reaction recapture failed");
        require(reaction.accumulate(0,GridIndex(1,1,1),2.0),"valid reaction rejected");
        const bool accepted = failure == 0
            ? reaction.accumulate(3,GridIndex(1,1,1),1.0)
            : failure == 1 ? reaction.accumulate(0,GridIndex(-1,1,1),1.0)
            : reaction.accumulate(0,GridIndex(1,1,1),std::numeric_limits<double>::quiet_NaN());
        require(!accepted && !reaction.finish() && !reaction.hasSolution(),
                "failed accumulation accepted a truncated reaction");
    }
    require(!reaction.prepare(std::numeric_limits<int>::max(),N,N,DX)
            && !reaction.beginCapture(N,N,N,DX,1000),"invalid preparation retained old storage eligibility");
    require(reaction.prepare(N,N,N,DX) && reaction.beginCapture(N,N,N,DX,1000)
            && reaction.finish() && reaction.impulse(0,GridIndex(1,1,1))==0.0,
            "reaction storage did not recover after invalid preparation");

    // Limits fixed before execution; conservation is relative to total boundary
    // exchange (net-only normalization proved ill-conditioned in f32).
    // This measures prescribed-boundary stress only.
    // It does not establish stable dynamic-body feedback or free-surface flow.
    require(result.max_linear_balance_error<1e-4,"viscous linear momentum does not balance");
    require(result.max_angular_balance_error<1e-4,"viscous angular momentum does not balance");
    require(result.max_rigid_velocity_error<2e-5 && result.max_rigid_impulse<1e-4,
            "rigid motion produced viscous strain");
    require(result.max_density_scaling_error<1e-6,"viscous impulse density scaling failed");
    require(result.max_passive_energy_ratio<=1.00001,"stationary viscous boundaries added energy");
    require(result.high_viscosity_energy_ratio<result.low_viscosity_energy_ratio,
            "higher viscosity did not dissipate more energy");
}

void run_viscous_feedback_probe(ManifoldViscousFeedbackProbe &result) {
    struct ThreadLimit {
        int previous=ThreadUtils::getMaxThreadCount();
        ThreadLimit() { ThreadUtils::setMaxThreadCount(2); }
        ~ThreadLimit() { ThreadUtils::setMaxThreadCount(previous); }
    } threads;
    result={};
    size_t index=0;
    // A frozen inner box moves only along X; the outer tank is fixed and the
    // liquid starts at rest. Other body DOFs are constrained and do no work.
    // One explicit reaction update must not add more than 1% total kinetic
    // energy. This is a rejection test, not a production exchange algorithm.
    for (double ratio : {0.1,1.0}) for (double nu : {1.0,10.0}) for (double dt : {DT,DT/2}) {
        Scene scene(nu,false,false,false);
        // This region contains every internal-obstacle solid face, with a
        // liquid gap separating it from all tank-wall solid faces.
        const auto inner=[](const Point &p) {
            return p[0]>.75 && p[0]<2.25 && p[1]>.75 && p[1]<2.25 && p[2]>.75 && p[2]<2.25;
        };
        each_face([&](int axis,GridIndex g,const Point &p) {
            const float value=axis==0 && !scene.is_fluid(axis,g) && inner(p) ? 1.0f : 0.0f;
            component(scene.velocity,axis).set(g,value);
        });
        ViscousBoundaryReaction reaction;
        require(reaction.prepare(N,N,N,DX),"viscous feedback storage preparation failed");
        ViscositySolver solver;
        require(solver.applyViscosityToVelocityField(scene.params(dt,1000,&reaction)),
                "viscous feedback candidate solve failed");
        double impulse=0.0, fluidEnergy=0.0;
        each_face([&](int axis,GridIndex g,const Point &p) {
            if (axis==0 && inner(p)) { impulse+=reaction.impulse(axis,g); }
            if (scene.solved(axis,g)) {
                const double v=component(scene.velocity,axis).get(g);
                fluidEnergy+=0.5*1000*DX*DX*DX*v*v;
            }
        });
        const double mass=1000*ratio*(2*0.38)*(2*0.44)*(2*0.32);
        const double velocity=1+impulse/mass;
        result.impulses[index]=impulse;
        result.energy_ratios[index]=(0.5*mass*velocity*velocity+fluidEnergy)/(0.5*mass);
        require(std::isfinite(impulse) && impulse<0 && std::isfinite(result.energy_ratios[index]),
                "viscous feedback candidate produced invalid measurements");
        ++index;
    }
}
