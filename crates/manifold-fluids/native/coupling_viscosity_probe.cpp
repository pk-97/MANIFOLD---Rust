#include "coupling_viscosity_probe.h"
#include "flip_engine/macvelocityfield.h"
#include "flip_engine/meshlevelset.h"
#include "flip_engine/particlelevelset.h"
#include "flip_engine/viscositysolver.h"
#include "flip_engine/viscousboundaryreaction.h"
#include "flip_engine/rigidboundaryvelocity.h"
#include "flip_engine/rigidviscositycoupling.h"
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

namespace {
using Dofs = RigidPressureCoupling::Dofs;
const Point inner_center = {1.47,1.56,1.40};
const Point inner_size = {0.76,0.88,0.64};

bool inner_face(const Point &p) {
    return p[0]>.75 && p[0]<2.25 && p[1]>.75 && p[1]<2.25 && p[2]>.75 && p[2]<2.25;
}

void prepare_inner_body(Scene &scene, RigidBoundaryVelocityMap &map,
                        const Dofs &velocity, MACVelocityField *scale) {
    map.prepare(N,N,N,DX,1,3*(N+1)*N*N);
    map.motions={{inner_center,velocity}};
    map.beginCapture();
    VelocityDataGrid data(N,N,N);
    each_face([&](int axis,GridIndex g,const Point &p) {
        double value=0.0;
        if (!scene.is_fluid(axis,g) && inner_face(p)) {
            value=map.sampleAndRecord(axis,g,0,1.0,p);
            (axis==0 ? data.weightU : axis==1 ? data.weightV : data.weightW).set(g,1.0f);
            component(data.field,axis).set(g,value);
            if (scale != nullptr) { value*=component(*scale,axis).get(g); }
        }
        component(scene.velocity,axis).set(g,value);
    });
    map.normalize(data);
    map.finish();
}

Point cuboid_inertia(double mass) {
    return {mass*(inner_size[1]*inner_size[1]+inner_size[2]*inner_size[2])/12,
            mass*(inner_size[0]*inner_size[0]+inner_size[2]*inner_size[2])/12,
            mass*(inner_size[0]*inner_size[0]+inner_size[1]*inner_size[1])/12};
}

void prepare_viscous_coupling(RigidViscosityCoupling &coupling,double mass,bool fixed=false) {
    coupling.reserve(1,3*N*N*N,6*N*N*N,24*N*N*N);
    RigidViscosityCoupling::Body body;
    if (!fixed) {
        body.inverseMass=1.0/mass;
        const Point inertia=cuboid_inertia(mass);
        for (int axis=0;axis<3;++axis) { body.inverseInertia[axis][axis]=1.0/inertia[axis]; }
    }
    coupling.bodies={body};
}

double body_energy(const Dofs &q,double mass) {
    const Point inertia=cuboid_inertia(mass);
    double energy=0.0;
    for (int axis=0;axis<3;++axis) {
        energy+=0.5*(mass*q[axis]*q[axis]+inertia[axis]*q[axis+3]*q[axis+3]);
    }
    return energy;
}

void measure_coupled(Scene &scene,RigidBoundaryVelocityMap &map,
                     RigidViscosityCoupling &coupling,ViscousBoundaryReaction &reaction,
                     const Dofs &initial,double mass,MACVelocityField *scale,
                     ManifoldCoupledViscosityProbe &result,
                     double surface=std::numeric_limits<double>::infinity()) {
    const Dofs &impulse=coupling.impulses()[0];
    const Dofs &change=coupling.velocityChanges()[0];
    const Dofs response=coupling.bodies[0].response(impulse);
    Dofs final=initial,transposed{};
    double fluidEnergy=0.0;
    each_face([&](int axis,GridIndex g,const Point &p) {
        if (scene.solved(axis,g)) {
            const double v=component(scene.velocity,axis).get(g);
            // Independent analytical mass for a horizontal free surface. A
            // face's staggered control volume is a dx-wide cube centred on p.
            const double fraction=std::clamp((surface-p[1]+0.5*DX)/DX,0.0,1.0);
            fluidEnergy+=0.5*1000*DX*DX*DX*fraction*v*v;
        }
        const double derivative=scale==nullptr ? 1.0 : component(*scale,axis).get(g);
        const double faceImpulse=derivative*reaction.impulse(axis,g);
        map.forEachFaceContribution(axis,g,[&](size_t body,const Dofs &basis) {
            require(body==0,"unexpected coupled viscosity probe body");
            for (int dof=0;dof<6;++dof) { transposed[dof]+=basis[dof]*faceImpulse; }
        });
    });
    for (int dof=0;dof<6;++dof) {
        require(std::isfinite(impulse[dof]) && std::isfinite(change[dof])
                && std::isfinite(response[dof]) && std::isfinite(transposed[dof]),
                "nonfinite coupled viscosity reaction");
        final[dof]+=change[dof];
        result.max_response_error=std::max(result.max_response_error,
            std::abs(change[dof]-response[dof])/std::max({1.0,std::abs(change[dof]),std::abs(response[dof])}));
        result.max_transpose_error=std::max(result.max_transpose_error,
            std::abs(impulse[dof]-transposed[dof])/std::max({1.0,std::abs(impulse[dof]),std::abs(transposed[dof])}));
    }
    const double ratio=(fluidEnergy+body_energy(final,mass))/body_energy(initial,mass);
    require(std::isfinite(ratio) && ratio>=0.0,"invalid coupled viscosity energy");
    result.max_energy_ratio=std::max(result.max_energy_ratio,ratio);
    if (std::isfinite(surface)) {
        result.max_free_surface_energy_ratio=std::max(result.max_free_surface_energy_ratio,ratio);
        ++result.free_surface_cases;
    }
    ++result.cases;
}
} // namespace

void run_coupled_viscosity_probe(ManifoldCoupledViscosityProbe &result) {
    struct ThreadLimit {
        int previous=ThreadUtils::getMaxThreadCount();
        ThreadLimit() { ThreadUtils::setMaxThreadCount(2); }
        ~ThreadLimit() { ThreadUtils::setMaxThreadCount(previous); }
    } threads;
    result={};
    RigidBoundaryVelocityMap map;
    RigidViscosityCoupling coupling;
    ViscousBoundaryReaction reaction;
    require(reaction.prepare(N,N,N,DX),"coupled viscosity reaction preparation failed");
    // Frozen geometry: the same light-body cases rejected by explicit feedback,
    // plus heavy bodies and rotation. All six body DOFs are free. The tank is
    // stationary, so viscosity must not add total fluid + body kinetic energy.
    for (bool rotation : {false,true}) for (double ratio : {0.1,1.0,10.0}) {
        for (double nu : {1.0,10.0}) for (double dt : {DT,DT/2}) {
            Scene scene(nu,false,false,false);
            const Dofs initial=rotation ? Dofs{0,0,0,0.4,-0.7,1.0} : Dofs{1,0,0,0,0,0};
            const double mass=1000*ratio*inner_size[0]*inner_size[1]*inner_size[2];
            prepare_inner_body(scene,map,initial,nullptr);
            prepare_viscous_coupling(coupling,mass);
            ViscositySolver solver;
            auto params=scene.params(dt,1000,&reaction);
            params.rigidCoupling=&coupling; params.rigidBoundaryMap=&map;
            require(solver.applyViscosityToVelocityField(params),"joint native viscosity solve failed");
            require(coupling.hasSolution() && reaction.hasSolution(),"missing coupled viscosity output");
            measure_coupled(scene,map,coupling,reaction,initial,mass,nullptr,result);
        }
    }

    // A spatially varying derivative exercises the existing constrained-field
    // chain rule: solid value = scale * S*q, reaction = S^T*scale*faceImpulse.
    Scene scaled(10,false,false,true);
    MACVelocityField scale(N,N,N,DX);
    each_face([&](int axis,GridIndex g,const Point &) {
        component(scale,axis).set(g,0.2+0.1*((g.i+2*g.j+g.k+axis)%7));
    });
    const Dofs initial={0.7,-0.4,0.2,0.4,-0.7,1.0};
    const double mass=100*inner_size[0]*inner_size[1]*inner_size[2];
    prepare_inner_body(scaled,map,initial,&scale);
    prepare_viscous_coupling(coupling,mass);
    ViscositySolver solver;
    auto params=scaled.params(DT,1000,&reaction);
    params.rigidCoupling=&coupling; params.rigidBoundaryMap=&map; params.rigidBoundaryScale=&scale;
    require(solver.applyViscosityToVelocityField(params),"scaled joint viscosity solve failed");
    measure_coupled(scaled,map,coupling,reaction,initial,mass,&scale,result);

    const Dofs scaledImpulse=coupling.impulses()[0];
    Scene noFaceOutput(10,false,false,true);
    prepare_inner_body(noFaceOutput,map,initial,&scale);
    params=noFaceOutput.params(DT,1000,nullptr);
    params.rigidCoupling=&coupling; params.rigidBoundaryMap=&map; params.rigidBoundaryScale=&scale;
    require(solver.applyViscosityToVelocityField(params),"coupled viscosity without face output failed");
    require(difference(scaled.velocity,noFaceOutput.velocity)<1e-7,
            "optional face reaction changed joint viscosity solve");
    for (int dof=0;dof<6;++dof) {
        require(coupling.impulses()[0][dof]==scaledImpulse[dof],
                "optional face reaction changed body impulse");
    }

    // The surface either cuts the box or lies just above it. Non-grid-aligned
    // heights exercise fractional face mass and dry stencil neighbours. The
    // body has simultaneous translation/rotation and one tenth liquid density.
    for (double surface : {1.63,2.0375}) for (double nu : {1.0,10.0}) {
        for (double dt : {DT,DT/2}) {
            Scene partial(nu,false,false,false);
            for (int k=0;k<N;++k) for (int j=0;j<N;++j) for (int i=0;i<N;++i) {
                partial.liquid.getPhiGrid()->set(i,j,k,(j+0.5)*DX-surface);
            }
            prepare_inner_body(partial,map,initial,nullptr);
            prepare_viscous_coupling(coupling,mass);
            params=partial.params(dt,1000,&reaction);
            params.rigidCoupling=&coupling; params.rigidBoundaryMap=&map;
            if (!solver.applyViscosityToVelocityField(params)) {
                throw std::runtime_error("free-surface joint viscosity failed: "+solver.getSolverStatus());
            }
            measure_coupled(partial,map,coupling,reaction,initial,mass,nullptr,result,surface);
        }
    }

    require(result.max_energy_ratio<=1.001,"coupled viscosity added passive kinetic energy");
    require(result.max_response_error<1e-4,"coupled viscosity body response differs from reaction");
    require(result.max_transpose_error<1e-4,"coupled viscosity reaction differs from boundary transpose");

    Scene fixed(1,false,false,false);
    prepare_inner_body(fixed,map,initial,nullptr);
    Scene baseline(1,false,false,false);
    baseline.velocity=fixed.velocity;
    prepare_viscous_coupling(coupling,mass,true);
    params=fixed.params(DT,1000,&reaction);
    params.rigidCoupling=&coupling; params.rigidBoundaryMap=&map;
    ViscositySolver ordinary;
    require(solver.applyViscosityToVelocityField(params)
            && ordinary.applyViscosityToVelocityField(baseline.params(DT,1000,nullptr)),
            "fixed-body viscosity baseline failed");
    result.fixed_velocity_error=difference(fixed.velocity,baseline.velocity);
    require(result.fixed_velocity_error<2e-6,"fixed mobility changed native viscosity solution");
    double reactionMagnitude=0.0;
    for (int dof=0;dof<6;++dof) {
        require(coupling.velocityChanges()[0][dof]==0.0,"fixed body gained viscosity velocity");
        reactionMagnitude+=std::abs(coupling.impulses()[0][dof]);
    }
    require(reactionMagnitude>1.0,"moving fixed boundary has no measured reaction");

    // Rejected requests never change fluid state or retain accepted reactions.
    const MACVelocityField before=fixed.velocity;
    auto unchanged=before;
    params.maxIterations=0;
    require(!solver.applyViscosityToVelocityField(params),"exhausted joint solve accepted native loose fallback");
    require(same_bits(fixed.velocity,unchanged) && !coupling.hasSolution() && !reaction.hasSolution(),
            "failed joint viscosity solve leaked state");
    params.maxIterations=900;
    RigidBoundaryVelocityMap wrongMap;
    wrongMap.prepare(N,N,N,DX,1,0); wrongMap.motions={{inner_center,initial}};
    params.rigidBoundaryMap=&wrongMap;
    rejects([&] { solver.applyViscosityToVelocityField(params); },"unfinished viscosity map accepted");
    require(same_bits(fixed.velocity,unchanged) && !coupling.hasSolution() && !reaction.hasSolution(),
            "bad viscosity map leaked state");
    params.rigidBoundaryMap=&map;
    MACVelocityField badScale(N+1,N,N,DX);
    params.rigidBoundaryScale=&badScale;
    rejects([&] { solver.applyViscosityToVelocityField(params); },"wrong viscosity derivative grid accepted");
    params.rigidBoundaryScale=nullptr;
    prepare_viscous_coupling(coupling,mass);
    fixed.liquid.getPhiGrid()->fill(1.0f);
    require(solver.applyViscosityToVelocityField(params),"empty coupled viscosity solve failed");
    require(coupling.hasSolution() && reaction.hasSolution() && same_bits(fixed.velocity,unchanged),
            "empty coupled viscosity capture lost state");
    for (double value : coupling.impulses()[0]) { require(value==0.0,"dry viscosity reaction is nonzero"); }
}
