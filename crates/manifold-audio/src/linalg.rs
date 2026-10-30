//! Single-precision matrix products for the CPU networks: one safe entry point, [`gemm`], over matrix views whose
//! bounds are checked when they are made. On macOS the products run on Apple's Accelerate, which uses the matrix
//! coprocessor; everywhere else on matrixmultiply, pure Rust with AVX and NEON kernels, so Linux and Windows build
//! with no system libraries. Both backends compile on every platform and are tested against the plain definition.

/// A `rows × cols` matrix inside a slice, element (r, c) at `r * row_stride + c * col_stride`, with one stride 1:
/// a row-major matrix or the transpose of one.
#[derive(Clone, Copy)]
pub(crate) struct Mat<'a> {
    data: &'a [f32],
    rows: usize,
    cols: usize,
    row_stride: usize,
    col_stride: usize,
}

impl<'a> Mat<'a> {
    /// Stored row by row, rows `row_stride` apart.
    pub(crate) fn rows(data: &'a [f32], rows: usize, cols: usize, row_stride: usize) -> Self {
        assert!(row_stride >= cols, "rows overlap");
        check(data.len(), rows, cols, (row_stride, 1));
        Self {
            data,
            rows,
            cols,
            row_stride,
            col_stride: 1,
        }
    }

    /// Stored column by column, columns `col_stride` apart: the transpose of a row-major matrix.
    pub(crate) fn cols(data: &'a [f32], rows: usize, cols: usize, col_stride: usize) -> Self {
        assert!(col_stride >= rows, "columns overlap");
        check(data.len(), rows, cols, (1, col_stride));
        Self {
            data,
            rows,
            cols,
            row_stride: 1,
            col_stride,
        }
    }
}

/// A row-major `rows × cols` matrix written in place, rows `row_stride` apart.
pub(crate) struct MatMut<'a> {
    data: &'a mut [f32],
    rows: usize,
    cols: usize,
    row_stride: usize,
}

impl<'a> MatMut<'a> {
    pub(crate) fn rows(data: &'a mut [f32], rows: usize, cols: usize, row_stride: usize) -> Self {
        assert!(row_stride >= cols, "rows overlap");
        check(data.len(), rows, cols, (row_stride, 1));
        Self {
            data,
            rows,
            cols,
            row_stride,
        }
    }
}

/// Every element of a `rows × cols` matrix with `strides` lies inside a slice of `len`.
fn check(len: usize, rows: usize, cols: usize, strides: (usize, usize)) {
    if rows > 0 && cols > 0 {
        let last = (rows - 1) * strides.0 + (cols - 1) * strides.1;
        assert!(
            last < len,
            "a {rows}×{cols} matrix runs past its {len} values"
        );
    }
}

/// c = a·b + beta·c.
pub(crate) fn gemm(a: Mat, b: Mat, beta: f32, c: MatMut) {
    assert!(
        a.cols == b.rows && a.rows == c.rows && b.cols == c.cols,
        "({}×{})·({}×{}) into {}×{}",
        a.rows,
        a.cols,
        b.rows,
        b.cols,
        c.rows,
        c.cols
    );
    if c.rows == 0 || c.cols == 0 {
        return;
    }
    #[cfg(target_os = "macos")]
    accelerate::gemm(a, b, beta, c);
    #[cfg(not(target_os = "macos"))]
    portable::gemm(a, b, beta, c);
}

#[cfg(target_os = "macos")]
mod accelerate {
    use super::{Mat, MatMut};

    const ROW_MAJOR: i32 = 101;
    const NO_TRANS: i32 = 111;
    const TRANS: i32 = 112;

    #[link(name = "Accelerate", kind = "framework")]
    unsafe extern "C" {
        fn cblas_sgemm(
            order: i32,
            trans_a: i32,
            trans_b: i32,
            m: i32,
            n: i32,
            k: i32,
            alpha: f32,
            a: *const f32,
            lda: i32,
            b: *const f32,
            ldb: i32,
            beta: f32,
            c: *mut f32,
            ldc: i32,
        );
    }

    /// The view as BLAS takes it: row-major, or transposed with its columns as rows.
    fn layout(m: &Mat) -> (i32, i32) {
        let blas_int = |v: usize| i32::try_from(v.max(1)).expect("a matrix BLAS can index");
        if m.col_stride == 1 {
            (NO_TRANS, blas_int(m.row_stride))
        } else {
            (TRANS, blas_int(m.col_stride))
        }
    }

    pub(super) fn gemm(a: Mat, b: Mat, beta: f32, c: MatMut) {
        let ((trans_a, lda), (trans_b, ldb)) = (layout(&a), layout(&b));
        let int = |v: usize| i32::try_from(v).expect("a matrix BLAS can index");
        // SAFETY: the views were checked to lie inside their slices with leading strides BLAS accepts, `gemm`
        // checked the shapes agree, and `c` is a unique borrow, so it aliases neither input.
        unsafe {
            cblas_sgemm(
                ROW_MAJOR,
                trans_a,
                trans_b,
                int(c.rows),
                int(c.cols),
                int(a.cols),
                1.0,
                a.data.as_ptr(),
                lda,
                b.data.as_ptr(),
                ldb,
                beta,
                c.data.as_mut_ptr(),
                int(c.row_stride.max(1)),
            );
        }
    }
}

#[cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]
mod portable {
    use super::{Mat, MatMut};

    pub(super) fn gemm(a: Mat, b: Mat, beta: f32, c: MatMut) {
        let int = |v: usize| isize::try_from(v).expect("a stride that fits isize");
        // SAFETY: the views were checked to lie inside their slices, `gemm` checked the shapes agree, and `c` is a
        // unique borrow, so it aliases neither input.
        unsafe {
            matrixmultiply::sgemm(
                c.rows,
                a.cols,
                c.cols,
                1.0,
                a.data.as_ptr(),
                int(a.row_stride),
                int(a.col_stride),
                b.data.as_ptr(),
                int(b.row_stride),
                int(b.col_stride),
                beta,
                c.data.as_mut_ptr(),
                int(c.row_stride),
                1,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Backend = fn(Mat, Mat, f32, MatMut);

    #[test]
    fn every_backend_and_layout_matches_the_definition() {
        let backends: Vec<(&str, Backend)> = vec![
            ("portable", portable::gemm),
            #[cfg(target_os = "macos")]
            ("accelerate", accelerate::gemm),
        ];
        let (m, k, n) = (5, 7, 3);
        let value = |i: usize| ((i * 37 % 23) as f32 - 11.0) / 7.0;
        // Each operand padded to a wider stride, stored both ways.
        let a_rows: Vec<f32> = (0..m * 9).map(value).collect();
        let b_rows: Vec<f32> = (0..k * 4).map(|i| value(i + 100)).collect();
        let a_at = |r: usize, c: usize| a_rows[r * 9 + c];
        let b_at = |r: usize, c: usize| b_rows[r * 4 + c];
        let mut a_cols = vec![0.0; k * 6];
        let mut b_cols = vec![0.0; n * 8];
        for r in 0..m {
            for c in 0..k {
                a_cols[c * 6 + r] = a_at(r, c);
            }
        }
        for r in 0..k {
            for c in 0..n {
                b_cols[c * 8 + r] = b_at(r, c);
            }
        }
        let start: Vec<f32> = (0..m * 4).map(|i| value(i + 200)).collect();
        for (name, backend) in &backends {
            for beta in [0.0, 1.0] {
                for a in [Mat::rows(&a_rows, m, k, 9), Mat::cols(&a_cols, m, k, 6)] {
                    for b in [Mat::rows(&b_rows, k, n, 4), Mat::cols(&b_cols, k, n, 8)] {
                        let mut c = start.clone();
                        backend(a, b, beta, MatMut::rows(&mut c, m, n, 4));
                        for r in 0..m {
                            for col in 0..n {
                                let want = beta * start[r * 4 + col]
                                    + (0..k).map(|i| a_at(r, i) * b_at(i, col)).sum::<f32>();
                                let got = c[r * 4 + col];
                                assert!((got - want).abs() < 1e-5, "{name}: {got} for {want}");
                            }
                            // The padding past each row stays untouched.
                            assert_eq!(c[r * 4 + n], start[r * 4 + n], "{name} wrote past a row");
                        }
                    }
                }
            }
        }
    }
}
