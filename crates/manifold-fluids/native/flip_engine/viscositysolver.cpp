/*
MIT License

Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/


/*
    Viscosity solver adapted from Christopher Batty's viscosity3d.cpp:
        https://github.com/christopherbatty/VariationalViscosity3D/blob/master/viscosity3d.cpp

    Accurate Viscous Free Surfaces for Buckling, Coiling, and Rotating Liquids
    C. Batty and R. Bridson
    http://www.cs.ubc.ca/nest/imager/tr/2008/Batty_ViscousFluids/viscosity.pdf
*/

#include "viscositysolver.h"

#include "threadutils.h"
#include "levelsetutils.h"
#include "macvelocityfield.h"
#include "particlelevelset.h"
#include "meshlevelset.h"
#include "interpolation.h"
#include "rigidviscositycoupling.h"
#include "rigidboundaryvelocity.h"

#include <cmath>
#include <limits>

ViscositySolver::ViscositySolver() {
}

ViscositySolver::~ViscositySolver() {
}

bool ViscositySolver::applyViscosityToVelocityField(ViscositySolverParameters params) {
    ViscousBoundaryReaction *boundaryReaction = params.boundaryReaction;
    struct ReactionCaptureGuard {
        ViscousBoundaryReaction *reaction;
        RigidViscosityCoupling *coupling;
        bool committed = false;
        ~ReactionCaptureGuard() {
            if (reaction != nullptr && !committed) {
                reaction->invalidate();
            }
            if (coupling != nullptr && !committed) { coupling->invalidate(); }
        }
    } reactionGuard{boundaryReaction,params.rigidCoupling};

    _initialize(params);
    _boundaryReaction = boundaryReaction;
    _rigidCoupling = params.rigidCoupling;
    _rigidBoundaryMap = params.rigidBoundaryMap;
    _rigidBoundaryScale = params.rigidBoundaryScale;
    if (_rigidCoupling != nullptr) {
        _solverStatus = "***Coupled viscosity solve incomplete";
        _rigidCoupling->invalidate();
        if (_rigidBoundaryMap == nullptr || !std::isfinite(params.reactionDensity)
            || params.reactionDensity <= 0.0) {
            throw std::invalid_argument("coupled viscosity requires a boundary map and physical density");
        }
        _rigidBoundaryMap->requireCompatible(_isize,_jsize,_ksize,_dx,_rigidCoupling->bodies.size());
        if (_rigidBoundaryScale != nullptr) {
            int ni,nj,nk;
            _rigidBoundaryScale->getGridDimensions(&ni,&nj,&nk);
            if (ni!=_isize || nj!=_jsize || nk!=_ksize) {
                throw std::invalid_argument("viscosity boundary derivative dimensions do not match");
            }
        }
    } else if (_rigidBoundaryMap != nullptr || _rigidBoundaryScale != nullptr) {
        throw std::invalid_argument("viscosity boundary derivative supplied without body coupling");
    }
    if (boundaryReaction != nullptr &&
        !boundaryReaction->beginCapture(_isize, _jsize, _ksize, _dx, params.reactionDensity)) {
        _solverStatus = "***Viscosity boundary reaction FAILED: invalid capture parameters";
        return false;
    }

    _computeFaceStateGrid();
    _computeVolumeGrid();
    if ((boundaryReaction != nullptr || _rigidCoupling != nullptr) && !_validateReactionInputs()) {
        _solverStatus = "***Viscosity boundary reaction FAILED: invalid native inputs";
        return false;
    }
    _computeMatrixIndexTable();

    int matsize = _matrixIndex.matrixSize;
    if (_rigidCoupling != nullptr) {
        const double cellMass=params.reactionDensity*static_cast<double>(_dx)*_dx*_dx;
        _rigidCoupling->beginCapture(matsize,cellMass);
        if (!_visitBoundaryTerms<float>(nullptr,params.reactionDensity)) {
            _solverStatus = "***Coupled viscosity FAILED: boundary stencil extraction";
            return false;
        }
        _rigidCoupling->finishCapture();
    }
    if (matsize == 0) {
        // Nothing to solve
        _solverIterations = 0;
        _solverError = 0.0f;
        _solverStatus = "Viscosity Solver Iterations: 0\nEstimated Error: 0.0";
        if (boundaryReaction != nullptr && !boundaryReaction->finish()) {
            boundaryReaction->invalidate();
            _solverStatus = "***Viscosity boundary reaction FAILED: output validation";
            return false;
        }
        if (_rigidCoupling != nullptr) { _rigidCoupling->captureEmptySolution(); }
        reactionGuard.committed = true;
        return true;
    }

    const bool success = _rigidCoupling != nullptr
        ? _solveAndApply<double>(params.reactionDensity)
        : _solveAndApply<float>(params.reactionDensity);
    reactionGuard.committed = success;
    return success;
}

std::string ViscositySolver::getSolverStatus() {
    return _solverStatus;
}

template <typename T>
bool ViscositySolver::_solveAndApply(double density) {
    const size_t systemSize = _rigidCoupling != nullptr
        ? _rigidCoupling->systemSize() : static_cast<size_t>(_matrixIndex.matrixSize);
    SparseMatrix<T> matrix(systemSize, 15);
    std::vector<T> rhs(systemSize, T(0));
    std::vector<T> soln(systemSize, T(0));

    _initializeLinearSystem(matrix, rhs);
    if (_rigidCoupling != nullptr) {
        _rigidCoupling->addMatrixDiagonalAndRhs(matrix, rhs);
    }

    if (!_solveLinearSystem(matrix, rhs, soln)) {
        if (_boundaryReaction != nullptr) {
            _boundaryReaction->invalidate();
        }
        return false;
    }

    if (_rigidCoupling != nullptr) {
        _rigidCoupling->captureSolution(soln);
    }
    if (_boundaryReaction != nullptr &&
        (!_visitBoundaryTerms(&soln, density) || !_boundaryReaction->finish())) {
        _boundaryReaction->invalidate();
        _solverStatus = "***Viscosity boundary reaction FAILED: stencil extraction";
        return false;
    }

    _applySolutionToVelocityField(soln);
    return true;
}

void ViscositySolver::_initialize(ViscositySolverParameters params) {
    int isize, jsize, ksize;
    params.velocityField->getGridDimensions(&isize, &jsize, &ksize);

    _isize = isize;
    _jsize = jsize;
    _ksize = ksize;
    _dx = params.cellwidth;
    _deltaTime = params.deltaTime;
    _velocityField = params.velocityField;
    _liquidSDF = params.liquidSDF;
    _solidSDF = params.solidSDF;
    _viscosity = params.viscosity;
    _solverTolerance = params.errorTolerance;
    _maxSolverIterations = params.maxIterations;
}

void ViscositySolver::_computeFaceStateGrid() {
    Array3d<float> solidCenterPhi(_isize, _jsize, _ksize);
    _computeSolidCenterPhi(solidCenterPhi);

    _state = FaceStateGrid(_isize, _jsize, _ksize);

    int U = 0; int V = 1; int W = 2;
    _computeFaceStateGridMT(solidCenterPhi, U);
    _computeFaceStateGridMT(solidCenterPhi, V);
    _computeFaceStateGridMT(solidCenterPhi, W);
}

void ViscositySolver::_computeFaceStateGridMT(Array3d<float> &solidCenterPhi, int dir) {
    int U = 0; int V = 1; int W = 2;

    size_t gridsize = 0;
    if (dir == U) {
        gridsize = _state.U.width * _state.U.height * _state.U.depth;
    } else if (dir == V) {
        gridsize = _state.V.width * _state.V.height * _state.V.depth;
    } else if (dir == W) {
        gridsize = _state.W.width * _state.W.height * _state.W.depth;
    }

    size_t numCPU = ThreadUtils::getMaxThreadCount();
    int numthreads = (int)std::min(numCPU, gridsize);
    std::vector<std::thread> threads(numthreads);
    std::vector<int> intervals = ThreadUtils::splitRangeIntoIntervals(0, gridsize, numthreads);
    for (int i = 0; i < numthreads; i++) {
        threads[i] = std::thread(&ViscositySolver::_computeFaceStateGridThread, this,
                                 intervals[i], intervals[i + 1], &solidCenterPhi, dir);
    }

    for (int i = 0; i < numthreads; i++) {
        threads[i].join();
    }
}

void ViscositySolver::_computeFaceStateGridThread(int startidx, int endidx, 
                                                  Array3d<float> *solidCenterPhi, int dir) {
    int U = 0; int V = 1; int W = 2;

    if (dir == U) {

        for (int idx = startidx; idx < endidx; idx++) {
            GridIndex g = Grid3d::getUnflattenedIndex(idx, _isize + 1, _jsize);
            bool isEdge = g.i == 0 || g.i == _state.U.width - 1;;
            if (isEdge || solidCenterPhi->get(g.i - 1, g.j, g.k) + solidCenterPhi->get(g.i, g.j, g.k) <= 0) {
                _state.U.set(g, FaceState::solid);
            } else { 
                _state.U.set(g, FaceState::fluid);
            }
        }

    } else if (dir == V) {

        for (int idx = startidx; idx < endidx; idx++) {
            GridIndex g = Grid3d::getUnflattenedIndex(idx, _isize, _jsize + 1);
            bool isEdge = g.j == 0 || g.j == _state.V.height - 1;
            if (isEdge || solidCenterPhi->get(g.i, g.j - 1, g.k) + solidCenterPhi->get(g.i, g.j, g.k) <= 0) {
                _state.V.set(g, FaceState::solid);
            } else { 
                _state.V.set(g, FaceState::fluid);
            }
        }

    } else if (dir == W) {

        for (int idx = startidx; idx < endidx; idx++) {
            GridIndex g = Grid3d::getUnflattenedIndex(idx, _isize, _jsize);
            bool isEdge = g.k == 0 || g.k == _state.W.depth - 1;
            if (isEdge || solidCenterPhi->get(g.i, g.j, g.k - 1) + solidCenterPhi->get(g.i, g.j, g.k) <= 0) {
                _state.W.set(g, FaceState::solid);
            } else { 
                _state.W.set(g, FaceState::fluid); 
            }
        }

    }
}

void ViscositySolver::_computeSolidCenterPhi(Array3d<float> &solidCenterPhi) {
    size_t gridsize = solidCenterPhi.width * solidCenterPhi.height * solidCenterPhi.depth;
    size_t numCPU = ThreadUtils::getMaxThreadCount();
    int numthreads = (int)std::min(numCPU, gridsize);
    std::vector<std::thread> threads(numthreads);
    std::vector<int> intervals = ThreadUtils::splitRangeIntoIntervals(0, gridsize, numthreads);
    for (int i = 0; i < numthreads; i++) {
        threads[i] = std::thread(&ViscositySolver::_computeSolidCenterPhiThread, this,
                                 intervals[i], intervals[i + 1], &solidCenterPhi);
    }

    for (int i = 0; i < numthreads; i++) {
        threads[i].join();
    }
}

void ViscositySolver::_computeSolidCenterPhiThread(int startidx, int endidx, 
                                                   Array3d<float> *solidCenterPhi) {
    int isize = solidCenterPhi->width;
    int jsize = solidCenterPhi->height;
    for (int idx = startidx; idx < endidx; idx++) {
        GridIndex g = Grid3d::getUnflattenedIndex(idx, isize, jsize);
        solidCenterPhi->set(g, _solidSDF->getDistanceAtCellCenter(g));
    }
}

void ViscositySolver::_computeVolumeGrid() {
    Array3d<bool> validCells(_isize + 1, _jsize + 1, _ksize + 1, false);
    for (int k = 0; k < _ksize; k++) {
        for (int j = 0; j < _jsize; j++) {
            for (int i = 0; i < _isize; i++) {
                if (_liquidSDF->get(i, j, k) < 0) {
                    validCells.set(i, j, k, true);
                }
            }
        }
    }

    int layers = 2;
    for (int layer = 0; layer < layers; layer++) {
        GridIndex nbs[6];
        Array3d<bool> tempValid = validCells;
        for (int k = 0; k < _ksize + 1; k++) {
            for (int j = 0; j < _jsize + 1; j++) {
                for (int i = 0; i < _isize + 1; i++) {
                    if (validCells(i, j, k)) {
                        Grid3d::getNeighbourGridIndices6(i, j, k, nbs);
                        for (int nidx = 0; nidx < 6; nidx++) {
                            if (tempValid.isIndexInRange(nbs[nidx])) {
                                tempValid.set(nbs[nidx], true);
                            }
                        }
                    }
                }
            }
        }
        validCells = tempValid;
    }

    if (_volumes.isize != _isize || _volumes.jsize != _jsize || _volumes.ksize != _ksize) {
        _volumes = ViscosityVolumeGrid(_isize, _jsize, _ksize);
        _subcellVolumeGrid = Array3d<float>(2 * _isize, 2 * _jsize, 2 * _ksize, 0.0f);
    } else {
        _volumes.clear();
        _subcellVolumeGrid.fill(0.0f);
    }

    vmath::vec3 centerStart(0.25f * _dx, 0.25f * _dx, 0.25f * _dx);
    _estimateVolumeFractions(&_subcellVolumeGrid, &validCells, centerStart, 0.5f * _dx);

    struct WorkGroup {
        Array3d<float> *grid;
        GridIndex gridOffset;
        WorkGroup(Array3d<float> *gridptr, GridIndex gridoffset) : 
                    grid(gridptr), gridOffset(gridoffset) {}
    };

    std::vector<WorkGroup> workqueue({
        WorkGroup(&(_volumes.center), GridIndex( 0,  0,  0)),
        WorkGroup(&(_volumes.U),      GridIndex(-1,  0,  0)),
        WorkGroup(&(_volumes.V),      GridIndex( 0, -1,  0)),
        WorkGroup(&(_volumes.W),      GridIndex( 0,  0, -1)),
        WorkGroup(&(_volumes.edgeU),  GridIndex( 0, -1, -1)),
        WorkGroup(&(_volumes.edgeV),  GridIndex(-1,  0, -1)),
        WorkGroup(&(_volumes.edgeW),  GridIndex(-1, -1,  0))
    });

    int numCPU = ThreadUtils::getMaxThreadCount();
    int numthreads = (int)fmin(numCPU, workqueue.size());
    std::vector<std::thread> threads(numthreads);

    while (!workqueue.empty()) {
        
        numthreads = (int)fmin(numCPU, workqueue.size());
        for (int tidx = 0; tidx < numthreads; tidx++) {
            WorkGroup workgroup = workqueue.back();
            workqueue.pop_back();

            threads[tidx] = std::thread(&ViscositySolver::_computeVolumeGridThread, this,
                                        workgroup.grid, &validCells, &_subcellVolumeGrid, 
                                        workgroup.gridOffset);
        }

        for (int tidx = 0; tidx < numthreads; tidx++) {
            threads[tidx].join();
        }
    }
}

void ViscositySolver::_estimateVolumeFractions(Array3d<float> *volumes, 
                                               Array3d<bool> *validCells, 
                                               vmath::vec3 centerStart, 
                                               float dx) {

    size_t gridsize = volumes->width * volumes->height * volumes->depth;
    size_t numCPU = ThreadUtils::getMaxThreadCount();
    int numthreads = (int)std::min(numCPU, gridsize);
    std::vector<std::thread> threads(numthreads);
    std::vector<int> intervals = ThreadUtils::splitRangeIntoIntervals(0, gridsize, numthreads);
    for (int i = 0; i < numthreads; i++) {
        threads[i] = std::thread(&ViscositySolver::_estimateVolumeFractionsThread, this,
                                 intervals[i], intervals[i + 1], 
                                 volumes, validCells, centerStart, dx);
    }

    for (int i = 0; i < numthreads; i++) {
        threads[i].join();
    }

}

void ViscositySolver::_estimateVolumeFractionsThread(int startidx, int endidx,
                                                     Array3d<float> *volumes, 
                                                     Array3d<bool> *validCells, 
                                                     vmath::vec3 centerStart, 
                                                     float dx) {

    int isize = volumes->width;
    int jsize = volumes->height;
    for (int idx = startidx; idx < endidx; idx++) {
        GridIndex g = Grid3d::getUnflattenedIndex(idx, isize, jsize);
        int i = g.i;
        int j = g.j;
        int k = g.k;

        if (!validCells->get(i / 2, j / 2, k / 2)) {
            continue;
        }

        vmath::vec3 center = centerStart + vmath::vec3(i * dx, j * dx, k * dx);
        float hdx = 0.5f * dx;

        float phi000 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(-hdx, -hdx, -hdx));
        float phi001 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(-hdx, -hdx, +hdx));
        float phi010 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(-hdx, +hdx, -hdx));
        float phi011 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(-hdx, +hdx, +hdx));
        float phi100 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(+hdx, -hdx, -hdx));
        float phi101 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(+hdx, -hdx, +hdx));
        float phi110 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(+hdx, +hdx, -hdx));
        float phi111 = _liquidSDF->trilinearInterpolate(center + vmath::vec3(+hdx, +hdx, +hdx));

        volumes->set(i, j, k, LevelsetUtils::volumeFraction(
                phi000, phi100, phi010, phi110, phi001, phi101, phi011, phi111
        ));
    }
}

void ViscositySolver::_computeVolumeGridThread(Array3d<float> *volumes, 
                                  Array3d<bool> *validCells,
                                  Array3d<float> *subcellVolumes,
                                  GridIndex gridOffset) {

    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                if (!validCells->get(i, j, k)) {
                    continue;
                }

                int base_i = 2 * i + gridOffset.i;
                int base_j = 2 * j + gridOffset.j;
                int base_k = 2 * k + gridOffset.k;
                for (int k_off = 0; k_off < 2; k_off++) {
                    for (int j_off = 0; j_off < 2; j_off++) {
                        for (int i_off = 0; i_off < 2; i_off++) {
                            volumes->add(i, j, k, subcellVolumes->get(base_i + i_off, base_j + j_off, base_k + k_off));
                        }
                    }
                }
                volumes->set(i, j, k, 0.125f * volumes->get(i, j, k));

            }
        }
    }
}

void ViscositySolver::_destroyVolumeGrid() {
    _volumes.destroy();
}

void ViscositySolver::_computeMatrixIndexTable() {

    int dim = (_isize + 1) * _jsize * _ksize + 
              _isize * (_jsize + 1) * _ksize + 
              _isize * _jsize * (_ksize + 1);
    FaceIndexer fidx(_isize, _jsize, _ksize);

    std::vector<bool> isIndexInMatrix(dim, false);
    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                if (_state.U(i, j, k) != FaceState::fluid) {
                    continue;
                }

                float v = _volumes.U(i, j, k);
                float vRight = _volumes.center(i, j, k);
                float vLeft = _volumes.center(i - 1, j, k);
                float vTop = _volumes.edgeW(i, j + 1, k);
                float vBottom = _volumes.edgeW(i, j, k);
                float vFront = _volumes.edgeV(i, j, k + 1);
                float vBack = _volumes.edgeV(i, j, k);

                if (v > 0.0 || vRight > 0.0 || vLeft > 0.0 || vTop > 0.0 || 
                        vBottom > 0.0 || vFront > 0.0 || vBack > 0.0) {
                    int index = fidx.U(i, j, k);
                    isIndexInMatrix[index] = true;
                }
            }
        }
    }

    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                if (_state.V(i, j, k) != FaceState::fluid) {
                    continue;
                }

                float v = _volumes.V(i, j, k);
                float vRight = _volumes.edgeW(i + 1, j, k);
                float vLeft = _volumes.edgeW(i, j, k);
                float vTop = _volumes.center(i, j, k);
                float vBottom = _volumes.center(i, j - 1, k);
                float vFront = _volumes.edgeU(i, j, k + 1);
                float vBack = _volumes.edgeU(i, j, k);

                if (v > 0.0 || vRight > 0.0 || vLeft > 0.0 || vTop > 0.0 || 
                        vBottom > 0.0 || vFront > 0.0 || vBack > 0.0) {
                    int index = fidx.V(i, j, k);
                    isIndexInMatrix[index] = true;
                }
            }
        }
    }

    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                if (_state.W(i, j, k) != FaceState::fluid) {
                    continue;
                }

                float v = _volumes.W(i, j, k);
                float vRight = _volumes.edgeV(i + 1, j, k);
                float vLeft = _volumes.edgeV(i, j, k);
                float vTop = _volumes.edgeU(i, j + 1, k);
                float vBottom = _volumes.edgeU(i, j, k);
                float vFront = _volumes.center(i, j, k);
                float vBack = _volumes.center(i, j, k - 1);

                if (v > 0.0 || vRight > 0.0 || vLeft > 0.0 || vTop > 0.0 || 
                        vBottom > 0.0 || vFront > 0.0 || vBack > 0.0) {
                    int index = fidx.W(i, j, k);
                    isIndexInMatrix[index] = true;
                }
            }
        }
    }

    std::vector<int> gridToMatrixIndex(dim, -1);
    int matrixindex = 0;
    for (size_t i = 0; i < isIndexInMatrix.size(); i++) {
        if (isIndexInMatrix[i]) {
            gridToMatrixIndex[i] = matrixindex;
            matrixindex++;
        }
    }

    _matrixIndex = MatrixIndexer(_isize, _jsize, _ksize, gridToMatrixIndex);
}

template <typename T>
void ViscositySolver::_initializeLinearSystem(SparseMatrix<T> &matrix, std::vector<T> &rhs) {
    _initializeLinearSystemU(matrix, rhs);
    _initializeLinearSystemV(matrix, rhs);
    _initializeLinearSystemW(matrix, rhs);
}

template <typename T>
void ViscositySolver::_initializeLinearSystemU(SparseMatrix<T> &matrix, std::vector<T> &rhs) {
    std::vector<GridIndex> indices;
    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                if(_state.U(i, j, k) == FaceState::fluid && _matrixIndex.U(i, j, k) != -1) {
                    indices.push_back(GridIndex(i, j, k));
                }
            }
        }
    }

    int numCPU = ThreadUtils::getMaxThreadCount();
    int numthreads = (int)fmin(numCPU, indices.size());
    std::vector<std::thread> threads(numthreads);
    std::vector<int> intervals = ThreadUtils::splitRangeIntoIntervals(0, indices.size(), numthreads);
    for (int i = 0; i < numthreads; i++) {
        threads[i] = std::thread(&ViscositySolver::_initializeLinearSystemThreadU<T>, this,
                                 intervals[i], intervals[i + 1], &indices, &matrix, &rhs);
    }

    for (int i = 0; i < numthreads; i++) {
        threads[i].join();
    }
}

template <typename T>
void ViscositySolver::_initializeLinearSystemV(SparseMatrix<T> &matrix, std::vector<T> &rhs) {
    std::vector<GridIndex> indices;
    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                if(_state.V(i, j, k) == FaceState::fluid && _matrixIndex.V(i, j, k) != -1) {
                    indices.push_back(GridIndex(i, j, k));
                }
            }
        }
    }

    int numCPU = ThreadUtils::getMaxThreadCount();
    int numthreads = (int)fmin(numCPU, indices.size());
    std::vector<std::thread> threads(numthreads);
    std::vector<int> intervals = ThreadUtils::splitRangeIntoIntervals(0, indices.size(), numthreads);
    for (int i = 0; i < numthreads; i++) {
        threads[i] = std::thread(&ViscositySolver::_initializeLinearSystemThreadV<T>, this,
                                 intervals[i], intervals[i + 1], &indices, &matrix, &rhs);
    }

    for (int i = 0; i < numthreads; i++) {
        threads[i].join();
    }
}

template <typename T>
void ViscositySolver::_initializeLinearSystemW(SparseMatrix<T> &matrix, std::vector<T> &rhs) {
    std::vector<GridIndex> indices;
    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                if(_state.W(i, j, k) == FaceState::fluid && _matrixIndex.W(i, j, k) != -1) {
                    indices.push_back(GridIndex(i, j, k));
                }
            }
        }
    }

    int numCPU = ThreadUtils::getMaxThreadCount();
    int numthreads = (int)fmin(numCPU, indices.size());
    std::vector<std::thread> threads(numthreads);
    std::vector<int> intervals = ThreadUtils::splitRangeIntoIntervals(0, indices.size(), numthreads);
    for (int i = 0; i < numthreads; i++) {
        threads[i] = std::thread(&ViscositySolver::_initializeLinearSystemThreadW<T>, this,
                                 intervals[i], intervals[i + 1], &indices, &matrix, &rhs);
    }

    for (int i = 0; i < numthreads; i++) {
        threads[i].join();
    }
}

template <typename T>
void ViscositySolver::_initializeLinearSystemThreadU(int startidx, int endidx,
                                                     std::vector<GridIndex> *indices,
                                                     SparseMatrix<T> *matrix,
                                                     std::vector<T> *rhs) {
    MatrixIndexer &mj = _matrixIndex;
    FaceState FLUID = FaceState::fluid;
    FaceState SOLID = FaceState::solid;

    float invdx = 1.0f / _dx;
    float factor = _deltaTime * invdx * invdx;
    for (int idx = startidx; idx < endidx; idx++) {
        int i = indices->at(idx).i;
        int j = indices->at(idx).j;
        int k = indices->at(idx).k;
        int row = _matrixIndex.U(i, j, k);

        float viscRight = _viscosity->get(i, j, k);
        float viscLeft = _viscosity->get(i - 1, j, k);

        float viscTop    = 0.25f * (_viscosity->get(i - 1, j + 1, k) + 
                                    _viscosity->get(i - 1, j,     k) + 
                                    _viscosity->get(i,     j + 1, k) + 
                                    _viscosity->get(i,     j,     k));
        float viscBottom = 0.25f * (_viscosity->get(i - 1, j,     k) + 
                                    _viscosity->get(i - 1, j - 1, k) + 
                                    _viscosity->get(i,     j,     k) + 
                                    _viscosity->get(i,     j - 1, k));

        float viscFront = 0.25f * (_viscosity->get(i - 1, j, k + 1) + 
                                   _viscosity->get(i - 1, j, k    ) + 
                                   _viscosity->get(i,     j, k + 1) + 
                                   _viscosity->get(i,     j, k    ));
        float viscBack  = 0.25f * (_viscosity->get(i - 1, j, k    ) + 
                                   _viscosity->get(i - 1, j, k - 1) + 
                                   _viscosity->get(i,     j, k    ) + 
                                   _viscosity->get(i,     j, k - 1));

        float volRight = _volumes.center(i, j, k);
        float volLeft = _volumes.center(i-1, j, k);
        float volTop = _volumes.edgeW(i, j + 1, k);
        float volBottom = _volumes.edgeW(i, j, k);
        float volFront = _volumes.edgeV(i, j, k + 1);
        float volBack = _volumes.edgeV(i, j, k);

        float factorRight  = 2 * factor * viscRight * volRight;
        float factorLeft   = 2 * factor * viscLeft * volLeft;
        float factorTop    = factor * viscTop * volTop;
        float factorBottom = factor * viscBottom * volBottom;
        float factorFront  = factor * viscFront * volFront;
        float factorBack   = factor * viscBack * volBack;

        T diag = static_cast<T>(_volumes.U(i, j, k))
               + static_cast<T>(factorRight) + static_cast<T>(factorLeft)
               + static_cast<T>(factorTop) + static_cast<T>(factorBottom)
               + static_cast<T>(factorFront) + static_cast<T>(factorBack);
        matrix->set(row, row, diag);
        if (_state.U(i + 1, j,     k    ) == FLUID) { matrix->add(row, mj.U(i + 1, j,     k    ), -factorRight ); }
        if (_state.U(i - 1, j,     k    ) == FLUID) { matrix->add(row, mj.U(i - 1, j,     k    ), -factorLeft  ); }
        if (_state.U(i,     j + 1, k    ) == FLUID) { matrix->add(row, mj.U(i,     j + 1, k    ), -factorTop   ); }
        if (_state.U(i,     j - 1, k    ) == FLUID) { matrix->add(row, mj.U(i,     j - 1, k    ), -factorBottom); }
        if (_state.U(i,     j,     k + 1) == FLUID) { matrix->add(row, mj.U(i,     j,     k + 1), -factorFront ); }
        if (_state.U(i,     j,     k - 1) == FLUID) { matrix->add(row, mj.U(i,     j,     k - 1), -factorBack  ); }

        if (_state.V(i,     j + 1, k    ) == FLUID) { matrix->add(row, mj.V(i,     j + 1, k    ), -factorTop   ); }
        if (_state.V(i - 1, j + 1, k    ) == FLUID) { matrix->add(row, mj.V(i - 1, j + 1, k    ),  factorTop   ); }
        if (_state.V(i,     j,     k    ) == FLUID) { matrix->add(row, mj.V(i,     j,     k    ),  factorBottom); }
        if (_state.V(i - 1, j,     k    ) == FLUID) { matrix->add(row, mj.V(i - 1, j,     k    ), -factorBottom); }
        
        if (_state.W(i,     j,     k + 1) == FLUID) { matrix->add(row, mj.W(i,     j,     k + 1), -factorFront ); }
        if (_state.W(i - 1, j,     k + 1) == FLUID) { matrix->add(row, mj.W(i - 1, j,     k + 1),  factorFront ); }
        if (_state.W(i,     j,     k    ) == FLUID) { matrix->add(row, mj.W(i,     j,     k    ),  factorBack  ); }
        if (_state.W(i - 1, j,     k    ) == FLUID) { matrix->add(row, mj.W(i - 1, j,     k    ), -factorBack  ); }

        T rval = static_cast<T>(_volumes.U(i, j, k))
               * static_cast<T>(_velocityField->U(i, j, k));
        if (_state.U(i + 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorRight)  * static_cast<T>(_velocityField->U(i + 1, j,     k    )); }
        if (_state.U(i - 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorLeft)   * static_cast<T>(_velocityField->U(i - 1, j,     k    )); }
        if (_state.U(i,     j + 1, k)     == SOLID) { rval -= static_cast<T>(-factorTop)    * static_cast<T>(_velocityField->U(i,     j + 1, k    )); }
        if (_state.U(i,     j - 1, k)     == SOLID) { rval -= static_cast<T>(-factorBottom) * static_cast<T>(_velocityField->U(i,     j - 1, k    )); }
        if (_state.U(i,     j,     k + 1) == SOLID) { rval -= static_cast<T>(-factorFront)  * static_cast<T>(_velocityField->U(i,     j,     k + 1)); }
        if (_state.U(i,     j,     k - 1) == SOLID) { rval -= static_cast<T>(-factorBack)   * static_cast<T>(_velocityField->U(i,     j,     k - 1)); }

        if (_state.V(i,     j + 1, k)     == SOLID) { rval -= static_cast<T>(-factorTop)    * static_cast<T>(_velocityField->V(i,     j + 1, k    )); }
        if (_state.V(i - 1, j + 1, k)     == SOLID) { rval -= static_cast<T>(factorTop)    * static_cast<T>(_velocityField->V(i - 1, j + 1, k    )); }
        if (_state.V(i,     j,     k)     == SOLID) { rval -= static_cast<T>(factorBottom) * static_cast<T>(_velocityField->V(i,     j,     k    )); }
        if (_state.V(i - 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorBottom) * static_cast<T>(_velocityField->V(i - 1, j,     k    )); }

        if (_state.W(i,     j,     k + 1) == SOLID) { rval -= static_cast<T>(-factorFront)  * static_cast<T>(_velocityField->W(i,     j,     k + 1)); }
        if (_state.W(i - 1, j,     k + 1) == SOLID) { rval -= static_cast<T>(factorFront)  * static_cast<T>(_velocityField->W(i - 1, j,     k + 1)); }
        if (_state.W(i,     j,     k)     == SOLID) { rval -= static_cast<T>(factorBack)   * static_cast<T>(_velocityField->W(i,     j,     k    )); }
        if (_state.W(i - 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorBack)   * static_cast<T>(_velocityField->W(i - 1, j,     k    )); }
        (*rhs)[row] = rval;
    }
}

template <typename T>
void ViscositySolver::_initializeLinearSystemThreadV(int startidx, int endidx,
                                                     std::vector<GridIndex> *indices,
                                                     SparseMatrix<T> *matrix,
                                                     std::vector<T> *rhs) {
    MatrixIndexer &mj = _matrixIndex;
    FaceState FLUID = FaceState::fluid;
    FaceState SOLID = FaceState::solid;

    float invdx = 1.0f / _dx;
    float factor = _deltaTime * invdx * invdx;
    for (int idx = startidx; idx < endidx; idx++) {
        int i = indices->at(idx).i;
        int j = indices->at(idx).j;
        int k = indices->at(idx).k;
        int row = _matrixIndex.V(i, j, k);  

        float viscRight = 0.25f * (_viscosity->get(i,     j - 1, k) + 
                                   _viscosity->get(i + 1, j - 1, k) + 
                                   _viscosity->get(i,     j,     k) + 
                                   _viscosity->get(i + 1, j,     k));
        float viscLeft  = 0.25f * (_viscosity->get(i,     j - 1, k) + 
                                   _viscosity->get(i - 1, j - 1, k) + 
                                   _viscosity->get(i,     j,     k) + 
                                   _viscosity->get(i - 1, j,     k));
        
        float viscTop = _viscosity->get(i, j, k);
        float viscBottom = _viscosity->get(i, j - 1, k);
        
        float viscFront = 0.25f * (_viscosity->get(i, j - 1, k    ) + 
                                   _viscosity->get(i, j - 1, k + 1) + 
                                   _viscosity->get(i, j,     k    ) + 
                                   _viscosity->get(i, j,     k + 1));
        float viscBack  = 0.25f * (_viscosity->get(i, j - 1, k    ) + 
                                   _viscosity->get(i, j - 1, k - 1) + 
                                   _viscosity->get(i, j,     k    ) + 
                                   _viscosity->get(i, j,     k - 1));

        float volRight = _volumes.edgeW(i + 1, j, k);
        float volLeft = _volumes.edgeW(i, j, k);
        float volTop = _volumes.center(i, j, k);
        float volBottom = _volumes.center(i, j - 1, k);
        float volFront = _volumes.edgeU(i, j, k + 1);
        float volBack = _volumes.edgeU(i, j, k);

        float factorRight  = factor * viscRight * volRight;
        float factorLeft   = factor * viscLeft * volLeft;
        float factorTop    = 2 * factor * viscTop * volTop;
        float factorBottom = 2 * factor * viscBottom * volBottom;
        float factorFront  = factor * viscFront * volFront;
        float factorBack   = factor * viscBack*volBack;

        T diag = static_cast<T>(_volumes.V(i, j, k))
               + static_cast<T>(factorRight) + static_cast<T>(factorLeft)
               + static_cast<T>(factorTop) + static_cast<T>(factorBottom)
               + static_cast<T>(factorFront) + static_cast<T>(factorBack);
        matrix->set(row, row, diag);
        if (_state.V(i + 1, j,     k    ) == FLUID) { matrix->add(row, mj.V(i + 1, j,     k    ), -factorRight ); }
        if (_state.V(i - 1, j,     k    ) == FLUID) { matrix->add(row, mj.V(i - 1, j,     k    ), -factorLeft  ); }
        if (_state.V(i,     j + 1, k    ) == FLUID) { matrix->add(row, mj.V(i,     j + 1, k    ), -factorTop   ); }
        if (_state.V(i,     j - 1, k    ) == FLUID) { matrix->add(row, mj.V(i,     j - 1, k    ), -factorBottom); }
        if (_state.V(i,     j,     k + 1) == FLUID) { matrix->add(row, mj.V(i,     j,     k + 1), -factorFront ); }
        if (_state.V(i,     j,     k - 1) == FLUID) { matrix->add(row, mj.V(i,     j,     k - 1), -factorBack  ); }

        if (_state.U(i + 1, j,     k    ) == FLUID) { matrix->add(row, mj.U(i + 1, j,     k    ), -factorRight ); }
        if (_state.U(i + 1, j - 1, k    ) == FLUID) { matrix->add(row, mj.U(i + 1, j - 1, k    ),  factorRight ); }
        if (_state.U(i,     j,     k    ) == FLUID) { matrix->add(row, mj.U(i,     j,     k    ),  factorLeft  ); }
        if (_state.U(i,     j - 1, k    ) == FLUID) { matrix->add(row, mj.U(i,     j - 1, k    ), -factorLeft  ); }
    
        if (_state.W(i,     j,     k + 1) == FLUID) { matrix->add(row, mj.W(i,     j,     k + 1), -factorFront ); }
        if (_state.W(i,     j - 1, k + 1) == FLUID) { matrix->add(row, mj.W(i,     j - 1, k + 1),  factorFront ); }
        if (_state.W(i,     j,     k    ) == FLUID) { matrix->add(row, mj.W(i,     j,     k    ),  factorBack  ); }
        if (_state.W(i,     j - 1, k    ) == FLUID) { matrix->add(row, mj.W(i,     j - 1, k    ), -factorBack  ); }

        T rval = static_cast<T>(_volumes.V(i, j, k))
               * static_cast<T>(_velocityField->V(i, j, k));
        if (_state.V(i + 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorRight)  * static_cast<T>(_velocityField->V(i + 1, j,     k    )); }
        if (_state.V(i - 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorLeft)   * static_cast<T>(_velocityField->V(i - 1, j,     k    )); }
        if (_state.V(i,     j + 1, k)     == SOLID) { rval -= static_cast<T>(-factorTop)    * static_cast<T>(_velocityField->V(i,     j + 1, k    )); }
        if (_state.V(i ,    j - 1, k)     == SOLID) { rval -= static_cast<T>(-factorBottom) * static_cast<T>(_velocityField->V(i,     j - 1, k    )); }
        if (_state.V(i    , j,     k + 1) == SOLID) { rval -= static_cast<T>(-factorFront)  * static_cast<T>(_velocityField->V(i,     j,     k + 1)); }
        if (_state.V(i,     j,     k - 1) == SOLID) { rval -= static_cast<T>(-factorBack)   * static_cast<T>(_velocityField->V(i,     j,     k - 1)); }

        if (_state.U(i + 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorRight)  * static_cast<T>(_velocityField->U(i + 1, j,     k    )); }
        if (_state.U(i + 1, j - 1, k)     == SOLID) { rval -= static_cast<T>(factorRight)  * static_cast<T>(_velocityField->U(i + 1, j - 1, k    )); }
        if (_state.U(i,     j,     k)     == SOLID) { rval -= static_cast<T>(factorLeft)   * static_cast<T>(_velocityField->U(i,     j,     k    )); }
        if (_state.U(i,     j - 1, k)     == SOLID) { rval -= static_cast<T>(-factorLeft)   * static_cast<T>(_velocityField->U(i,     j - 1, k    )); }

        if (_state.W(i,     j,     k + 1) == SOLID) { rval -= static_cast<T>(-factorFront)  * static_cast<T>(_velocityField->W(i,     j,     k + 1)); }
        if (_state.W(i,     j - 1, k + 1) == SOLID) { rval -= static_cast<T>(factorFront)  * static_cast<T>(_velocityField->W(i,     j - 1, k + 1)); }
        if (_state.W(i,     j,     k)     == SOLID) { rval -= static_cast<T>(factorBack)   * static_cast<T>(_velocityField->W(i,     j,     k    )); }
        if (_state.W(i,     j - 1, k)     == SOLID) { rval -= static_cast<T>(-factorBack)   * static_cast<T>(_velocityField->W(i,     j - 1, k    )); }
        (*rhs)[row] = rval;
    }
}

template <typename T>
void ViscositySolver::_initializeLinearSystemThreadW(int startidx, int endidx,
                                                     std::vector<GridIndex> *indices,
                                                     SparseMatrix<T> *matrix,
                                                     std::vector<T> *rhs) {
    MatrixIndexer &mj = _matrixIndex;
    FaceState FLUID = FaceState::fluid;
    FaceState SOLID = FaceState::solid;

    float invdx = 1.0f / _dx;
    float factor = _deltaTime * invdx * invdx;
    for (int idx = startidx; idx < endidx; idx++) {
        int i = indices->at(idx).i;
        int j = indices->at(idx).j;
        int k = indices->at(idx).k;
        int row = _matrixIndex.W(i, j, k);

        float viscRight = 0.25f * (_viscosity->get(i,     j, k    ) + 
                                   _viscosity->get(i,     j, k - 1) + 
                                   _viscosity->get(i + 1, j, k    ) + 
                                   _viscosity->get(i + 1, j, k - 1));
        float viscLeft  = 0.25f * (_viscosity->get(i,     j, k    ) + 
                                   _viscosity->get(i,     j, k - 1) + 
                                   _viscosity->get(i - 1, j, k    ) + 
                                   _viscosity->get(i - 1, j, k - 1));

        float viscTop    = 0.25f * (_viscosity->get(i, j,     k    ) + 
                                    _viscosity->get(i, j,     k - 1) + 
                                    _viscosity->get(i, j + 1, k    ) + 
                                    _viscosity->get(i, j + 1, k - 1));
        float viscBottom = 0.25f * (_viscosity->get(i, j,     k    ) + 
                                    _viscosity->get(i, j,     k - 1) + 
                                    _viscosity->get(i, j - 1, k    ) + 
                                    _viscosity->get(i, j - 1, k - 1));

        float viscFront = _viscosity->get(i, j, k);   
        float viscBack = _viscosity->get(i, j, k - 1); 

        float volRight = _volumes.edgeV(i + 1, j, k);
        float volLeft = _volumes.edgeV(i, j, k);
        float volTop = _volumes.edgeU(i, j + 1, k);
        float volBottom = _volumes.edgeU(i, j, k);
        float volFront = _volumes.center(i, j, k);
        float volBack = _volumes.center(i, j, k - 1);

        float factorRight  = factor * viscRight * volRight;
        float factorLeft   = factor * viscLeft * volLeft;
        float factorTop    = factor * viscTop * volTop;
        float factorBottom = factor * viscBottom * volBottom;
        float factorFront  = 2 * factor * viscFront * volFront;
        float factorBack   = 2 * factor * viscBack*volBack;

        T diag = static_cast<T>(_volumes.W(i, j, k))
               + static_cast<T>(factorRight) + static_cast<T>(factorLeft)
               + static_cast<T>(factorTop) + static_cast<T>(factorBottom)
               + static_cast<T>(factorFront) + static_cast<T>(factorBack);
        matrix->set(row, row, diag);
        if (_state.W(i + 1, j,     k    ) == FLUID) { matrix->add(row, mj.W(i + 1, j,     k    ), -factorRight ); }
        if (_state.W(i - 1, j,     k    ) == FLUID) { matrix->add(row, mj.W(i - 1, j,     k    ), -factorLeft  ); }
        if (_state.W(i,     j + 1, k    ) == FLUID) { matrix->add(row, mj.W(i,     j + 1, k    ), -factorTop   ); }
        if (_state.W(i,     j - 1, k    ) == FLUID) { matrix->add(row, mj.W(i,     j - 1, k    ), -factorBottom); }
        if (_state.W(i,     j,     k + 1) == FLUID) { matrix->add(row, mj.W(i,     j,     k + 1), -factorFront ); }
        if (_state.W(i,     j,     k - 1) == FLUID) { matrix->add(row, mj.W(i,     j,     k - 1), -factorBack  ); }

        if (_state.U(i + 1, j,     k    ) == FLUID) { matrix->add(row, mj.U(i + 1, j,     k    ), -factorRight ); } 
        if (_state.U(i + 1, j,     k - 1) == FLUID) { matrix->add(row, mj.U(i + 1, j,     k - 1),  factorRight ); }
        if (_state.U(i,     j,     k    ) == FLUID) { matrix->add(row, mj.U(i,     j,     k    ),  factorLeft  ); }
        if (_state.U(i,     j,     k - 1) == FLUID) { matrix->add(row, mj.U(i,     j,     k - 1), -factorLeft  ); }
        
        if (_state.V(i,     j + 1, k    ) == FLUID) { matrix->add(row, mj.V(i,     j + 1, k    ), -factorTop   ); }
        if (_state.V(i,     j + 1, k - 1) == FLUID) { matrix->add(row, mj.V(i,     j + 1, k - 1),  factorTop   ); }
        if (_state.V(i,     j,     k    ) == FLUID) { matrix->add(row, mj.V(i,     j,     k    ),  factorBottom); }
        if (_state.V(i,     j,     k - 1) == FLUID) { matrix->add(row, mj.V(i,     j,     k - 1), -factorBottom); }

        T rval = static_cast<T>(_volumes.W(i, j, k))
               * static_cast<T>(_velocityField->W(i, j, k));
        if (_state.W(i + 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorRight)  * static_cast<T>(_velocityField->W(i + 1, j,     k    )); }
        if (_state.W(i - 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorLeft)   * static_cast<T>(_velocityField->W(i - 1, j,     k    )); }
        if (_state.W(i,     j + 1, k)     == SOLID) { rval -= static_cast<T>(-factorTop)    * static_cast<T>(_velocityField->W(i,     j + 1, k    )); }
        if (_state.W(i,     j - 1, k)     == SOLID) { rval -= static_cast<T>(-factorBottom) * static_cast<T>(_velocityField->W(i,     j - 1, k    )); }
        if (_state.W(i,     j,     k + 1) == SOLID) { rval -= static_cast<T>(-factorFront)  * static_cast<T>(_velocityField->W(i,     j,     k + 1)); }
        if (_state.W(i,     j,     k - 1) == SOLID) { rval -= static_cast<T>(-factorBack)   * static_cast<T>(_velocityField->W(i,     j,     k - 1)); }

        if (_state.U(i + 1, j,     k)     == SOLID) { rval -= static_cast<T>(-factorRight)  * static_cast<T>(_velocityField->U(i + 1, j,     k    )); }
        if (_state.U(i + 1, j,     k - 1) == SOLID) { rval -= static_cast<T>(factorRight)  * static_cast<T>(_velocityField->U(i + 1, j,     k - 1)); }
        if (_state.U(i,     j,     k)     == SOLID) { rval -= static_cast<T>(factorLeft)   * static_cast<T>(_velocityField->U(i,     j,     k    )); }
        if (_state.U(i,     j,     k - 1) == SOLID) { rval -= static_cast<T>(-factorLeft)   * static_cast<T>(_velocityField->U(i,     j,     k - 1)); }

        if (_state.V(i,     j + 1, k)     == SOLID) { rval -= static_cast<T>(-factorTop)    * static_cast<T>(_velocityField->V(i,     j + 1, k    )); }
        if (_state.V(i,     j + 1, k - 1) == SOLID) { rval -= static_cast<T>(factorTop)    * static_cast<T>(_velocityField->V(i,     j + 1, k - 1)); }
        if (_state.V(i,     j,     k)     == SOLID) { rval -= static_cast<T>(factorBottom) * static_cast<T>(_velocityField->V(i,     j,     k    )); }
        if (_state.V(i,     j,     k - 1) == SOLID) { rval -= static_cast<T>(-factorBottom) * static_cast<T>(_velocityField->V(i,     j,     k - 1)); }
        (*rhs)[row] = rval;
    }
}

template <typename T>
bool ViscositySolver::_solveLinearSystem(SparseMatrix<T> &matrix, std::vector<T> &rhs,
                                         std::vector<T> &soln) {

    PCGSolver<T> solver;
    solver.setSolverParameters(_solverTolerance, _maxSolverIterations);

    T estimatedError;
    int numIterations;
    bool success = _rigidCoupling != nullptr
        ? solver.solveWithAdditionalMatrix(matrix,rhs,soln,estimatedError,numIterations,
            [&](const std::vector<T> &x,std::vector<T> &y) {
                _rigidCoupling->addRemainingMatrixProduct(x,y);
            })
        : solver.solve(matrix, rhs, soln, estimatedError, numIterations);
    _solverIterations = numIterations;
    _solverError = (float)estimatedError;

    bool retval;
    std::ostringstream ss;
    if (success) {
        ss << "Viscosity Solver Iterations: " << numIterations <<
              "\nEstimated Error: " << estimatedError;
        retval = true;
    } else if (_rigidCoupling == nullptr && numIterations == _maxSolverIterations && estimatedError < _acceptableTolerace) {
        ss << "Viscosity Solver Iterations: " << numIterations <<
              "\nEstimated Error: " << estimatedError;
        retval = true;
    } else {
        ss << "***Viscosity Solver FAILED" <<
              "\nViscosity Solver Iterations: " << numIterations <<
              "\nEstimated Error: " << estimatedError;
        retval = false;
    }

    _solverStatus = ss.str();

    return retval;
}

bool ViscositySolver::_validateReactionInputs() {
    if (!std::isfinite(_dx) || _dx <= 0.0f ||
        !std::isfinite(_deltaTime) || _deltaTime <= 0.0f || _viscosity == nullptr) {
        return false;
    }

    const auto finiteNonnegative = [](Array3d<float> &grid) {
        const size_t count = static_cast<size_t>(grid.width) * grid.height * grid.depth;
        const float *values = grid.getRawArray();
        for (size_t index = 0; index < count; ++index) {
            if (!std::isfinite(values[index]) || values[index] < 0.0f) { return false; }
        }
        return true;
    };
    const auto finiteVelocity = [](Array3d<float> &grid) {
        const size_t count = static_cast<size_t>(grid.width) * grid.height * grid.depth;
        const float *values = grid.getRawArray();
        for (size_t index = 0; index < count; ++index) {
            if (!std::isfinite(values[index])) { return false; }
        }
        return true;
    };
    return finiteVelocity(*_velocityField->getArray3dU())
        && finiteVelocity(*_velocityField->getArray3dV())
        && finiteVelocity(*_velocityField->getArray3dW())
        && finiteNonnegative(*_viscosity) && finiteNonnegative(_volumes.center)
        && finiteNonnegative(_volumes.U) && finiteNonnegative(_volumes.V)
        && finiteNonnegative(_volumes.W) && finiteNonnegative(_volumes.edgeU)
        && finiteNonnegative(_volumes.edgeV) && finiteNonnegative(_volumes.edgeW);
}

template <typename T>
bool ViscositySolver::_getReactionFaceValue(int axis, GridIndex g,
                                             const std::vector<T> &soln,
                                             double *value) {
    FaceState state;
    int matrixIndex;
    if (axis == 0) {
        state = _state.U(g);
        matrixIndex = _matrixIndex.U(g.i, g.j, g.k);
    } else if (axis == 1) {
        state = _state.V(g);
        matrixIndex = _matrixIndex.V(g.i, g.j, g.k);
    } else if (axis == 2) {
        state = _state.W(g);
        matrixIndex = _matrixIndex.W(g.i, g.j, g.k);
    } else {
        return false;
    }

    if (state == FaceState::fluid) {
        if (matrixIndex < 0 || static_cast<size_t>(matrixIndex) >= soln.size()) {
            return false;
        }
        *value = soln[matrixIndex];
    } else if (state == FaceState::solid) {
        if (axis == 0) {
            *value = _velocityField->U(g);
        } else if (axis == 1) {
            *value = _velocityField->V(g);
        } else {
            *value = _velocityField->W(g);
        }
        if (_rigidCoupling != nullptr) {
            // Accepted changes were validated once during solution capture.
            // Visit only this face's contributors, avoiding an all-body scan
            // for every face in the viscous stress stencil.
            const auto &changes=_rigidCoupling->velocityChanges();
            double correction=0.0;
            _rigidBoundaryMap->forEachFaceContribution(axis,g,
                [&](size_t body,const RigidPressureCoupling::Dofs &basis) {
                    for (int dof=0;dof<6;++dof) { correction+=basis[dof]*changes[body][dof]; }
                });
            *value += _rigidFaceScale(axis,g)*correction;
        }
    } else {
        return false;
    }
    return std::isfinite(*value);
}

double ViscositySolver::_rigidFaceScale(int axis,GridIndex g) const {
    if (_rigidBoundaryScale == nullptr) { return 1.0; }
    const double scale=axis==0 ? _rigidBoundaryScale->U(g)
        : axis==1 ? _rigidBoundaryScale->V(g) : _rigidBoundaryScale->W(g);
    if (!std::isfinite(scale) || scale<0.0 || scale>1.0) {
        throw std::invalid_argument("viscosity boundary derivative must be between zero and one");
    }
    return scale;
}

template <typename T>
bool ViscositySolver::_captureReactionTerm(const GridIndex *faces, const int *axes,
                                           const int *signs, int count, float weight,
                                           const std::vector<T> *soln, double density) {
    if (!std::isfinite(weight)) {
        return false;
    }
    if (weight <= 0.0f) {
        return true;
    }

    bool hasActiveFluid = false;
    std::array<int,4> fluidRows{{-1,-1,-1,-1}};
    std::array<double,4> fluidCoefficients{};
    std::array<FaceState,4> states{};
    for (int n = 0; n < count; n++) {
        int matrixIndex = -1;
        FaceState state;
        if (axes[n] == 0) {
            state = _state.U(faces[n]);
            matrixIndex = _matrixIndex.U(faces[n].i, faces[n].j, faces[n].k);
        } else if (axes[n] == 1) {
            state = _state.V(faces[n]);
            matrixIndex = _matrixIndex.V(faces[n].i, faces[n].j, faces[n].k);
        } else if (axes[n] == 2) {
            state = _state.W(faces[n]);
            matrixIndex = _matrixIndex.W(faces[n].i, faces[n].j, faces[n].k);
        } else {
            return false;
        }
        states[n]=state;
        if (state == FaceState::fluid && matrixIndex >= 0) {
            hasActiveFluid = true;
            fluidRows[n]=matrixIndex;
            fluidCoefficients[n]=signs[n];
        }
    }
    if (!hasActiveFluid) {
        return true;
    }

    if (soln == nullptr) {
        double prescribed=0.0;
        for (int n=0;n<count;++n) {
            if (states[n] == FaceState::solid) {
                const double value=axes[n]==0 ? _velocityField->U(faces[n])
                    : axes[n]==1 ? _velocityField->V(faces[n]) : _velocityField->W(faces[n]);
                prescribed+=signs[n]*value;
            } else if (fluidRows[n]<0) { return false; }
        }
        _rigidCoupling->beginTerm(weight,prescribed,fluidRows,fluidCoefficients);
        for (int n=0;n<count;++n) {
            if (states[n] != FaceState::solid) { continue; }
            const double scale=signs[n]*_rigidFaceScale(axes[n],faces[n]);
            _rigidBoundaryMap->forEachFaceContribution(axes[n],faces[n],
                [&](size_t body,const RigidPressureCoupling::Dofs &basis) {
                    auto derivative=basis;
                    for (double &value : derivative) { value*=scale; }
                    _rigidCoupling->addBody(body,derivative);
                });
        }
        _rigidCoupling->endTerm();
        return true;
    }

    double values[4] = {0.0, 0.0, 0.0, 0.0};
    double strain = 0.0;
    for (int n = 0; n < count; n++) {
        if (!_getReactionFaceValue(axes[n], faces[n], *soln, &values[n])) {
            return false;
        }
        strain += static_cast<double>(signs[n]) * values[n];
    }
    if (!std::isfinite(strain)) {
        return false;
    }

    const double impulseScale = density * static_cast<double>(_dx) * _dx * _dx * weight;
    if (!std::isfinite(impulseScale)) {
        return false;
    }
    for (int n = 0; n < count; n++) {
        FaceState state;
        if (axes[n] == 0) {
            state = _state.U(faces[n]);
        } else if (axes[n] == 1) {
            state = _state.V(faces[n]);
        } else {
            state = _state.W(faces[n]);
        }
        if (state == FaceState::solid) {
            const double impulse = -impulseScale * signs[n] * strain;
            if (!std::isfinite(impulse) || !_boundaryReaction->accumulate(axes[n], faces[n], impulse)) {
                return false;
            }
        }
    }
    return true;
}

template <typename T>
bool ViscositySolver::_visitBoundaryTerms(const std::vector<T> *soln, double density) {
    if ((soln != nullptr && _boundaryReaction == nullptr) || !std::isfinite(density) || density <= 0.0) {
        return false;
    }

    const float invdx = 1.0f / _dx;
    const float factor = _deltaTime * invdx * invdx;
    for (int k = 1; k < _ksize; k++) {
        for (int j = 1; j < _jsize; j++) {
            for (int i = 1; i < _isize; i++) {
                GridIndex normalFaces[2] = {GridIndex(i + 1, j, k), GridIndex(i, j, k)};
                int normalAxes[2] = {0, 0};
                const int normalSigns[2] = {1, -1};
                float weight = 2.0f * factor * _viscosity->get(i, j, k) * _volumes.center(i, j, k);
                if (!_captureReactionTerm<T>(normalFaces, normalAxes, normalSigns, 2,
                                          weight, soln, density)) {
                    return false;
                }

                normalFaces[0] = GridIndex(i, j + 1, k);
                normalFaces[1] = GridIndex(i, j, k);
                normalAxes[0] = 1;
                normalAxes[1] = 1;
                weight = 2.0f * factor * _viscosity->get(i, j, k) * _volumes.center(i, j, k);
                if (!_captureReactionTerm<T>(normalFaces, normalAxes, normalSigns, 2,
                                          weight, soln, density)) {
                    return false;
                }

                normalFaces[0] = GridIndex(i, j, k + 1);
                normalFaces[1] = GridIndex(i, j, k);
                normalAxes[0] = 2;
                normalAxes[1] = 2;
                weight = 2.0f * factor * _viscosity->get(i, j, k) * _volumes.center(i, j, k);
                if (!_captureReactionTerm<T>(normalFaces, normalAxes, normalSigns, 2,
                                          weight, soln, density)) {
                    return false;
                }

                GridIndex shearFaces[4] = {
                    GridIndex(i, j, k), GridIndex(i, j - 1, k),
                    GridIndex(i, j, k), GridIndex(i - 1, j, k)};
                const int shearAxesW[4] = {0, 0, 1, 1};
                const int shearSigns[4] = {1, -1, 1, -1};
                const float viscW = 0.25f * (_viscosity->get(i - 1, j, k) +
                                             _viscosity->get(i - 1, j - 1, k) +
                                             _viscosity->get(i, j, k) +
                                             _viscosity->get(i, j - 1, k));
                weight = factor * viscW * _volumes.edgeW(i, j, k);
                if (!_captureReactionTerm<T>(shearFaces, shearAxesW, shearSigns, 4,
                                          weight, soln, density)) {
                    return false;
                }

                shearFaces[1] = GridIndex(i, j, k - 1);
                shearFaces[3] = GridIndex(i - 1, j, k);
                const int shearAxesV[4] = {0, 0, 2, 2};
                const float viscV = 0.25f * (_viscosity->get(i - 1, j, k) +
                                             _viscosity->get(i - 1, j, k - 1) +
                                             _viscosity->get(i, j, k) +
                                             _viscosity->get(i, j, k - 1));
                weight = factor * viscV * _volumes.edgeV(i, j, k);
                if (!_captureReactionTerm<T>(shearFaces, shearAxesV, shearSigns, 4,
                                          weight, soln, density)) {
                    return false;
                }

                shearFaces[0] = GridIndex(i, j, k);
                shearFaces[1] = GridIndex(i, j, k - 1);
                shearFaces[2] = GridIndex(i, j, k);
                shearFaces[3] = GridIndex(i, j - 1, k);
                const int shearAxesU[4] = {1, 1, 2, 2};
                const float viscU = 0.25f * (_viscosity->get(i, j - 1, k) +
                                             _viscosity->get(i, j - 1, k - 1) +
                                             _viscosity->get(i, j, k) +
                                             _viscosity->get(i, j, k - 1));
                weight = factor * viscU * _volumes.edgeU(i, j, k);
                if (!_captureReactionTerm<T>(shearFaces, shearAxesU, shearSigns, 4,
                                          weight, soln, density)) {
                    return false;
                }
            }
        }
    }
    return true;
}

template <typename T>
void ViscositySolver::_applySolutionToVelocityField(const std::vector<T> &soln) {
    for (T value : soln) {
        if (!std::isfinite(value) || !std::isfinite(static_cast<float>(value))) {
            throw std::invalid_argument("viscosity solution is outside native velocity range");
        }
    }
    _velocityField->clear();
    for(int k = 0; k < _ksize; k++) {
        for(int j = 0; j < _jsize; j++) {
            for(int i = 0; i < _isize + 1; i++) {
                int matidx = _matrixIndex.U(i, j, k);
                if (matidx != -1) {
                    _velocityField->setU(i, j, k, soln[matidx]);
                }
            }
        }
    }

    for(int k = 0; k < _ksize; k++) {
        for(int j = 0; j < _jsize + 1; j++) {
            for(int i = 0; i < _isize; i++) {
                int matidx = _matrixIndex.V(i, j, k);
                if (matidx != -1) {
                    _velocityField->setV(i, j, k, soln[matidx]);
                }
            }
        }
    }

    for(int k = 0; k < _ksize + 1; k++) {
        for(int j = 0; j < _jsize; j++) {
            for(int i = 0; i < _isize; i++) {
                int matidx = _matrixIndex.W(i, j, k);
                if (matidx != -1) {
                    _velocityField->setW(i, j, k, soln[matidx]);
                }
            }
        }
    }

}
