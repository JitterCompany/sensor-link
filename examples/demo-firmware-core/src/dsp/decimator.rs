/// Decimator: downsamples data by skipping samples: every n-th sample is passed through
/// and the rest is discarded.
///
/// This method of downsampling assumes the input data does not contain significant
/// signal components above fs/(2*decimation_factor) (f_nyquist of the output signal).
/// Apply a low-pass (anti-alias) filter with suitable attenuation above fs/(2*decimation_factor)
/// to minimize aliasing noise.
pub struct Decimator {
    decimation_factor: u32,

    /// Decimator state: counts up to `decimation_factor` to determine when a sample
    /// is passed through.
    decimator_count: u32,
}

/// An iterator that decimates samples from the input iterator
///
/// This struct is created by [Decimator::decimate](method@Decimator::decimate)
pub struct DecimIter<'decim, I: IntoIterator> {
    sample_iter: core::iter::Enumerate<<I as IntoIterator>::IntoIter>,

    decimator: &'decim mut Decimator,
}

impl<'decim, I> Iterator for DecimIter<'decim, I>
where
    I: IntoIterator<Item = f32>,
{
    type Item = (usize, f32);

    fn next(&mut self) -> Option<Self::Item> {
        for (index, sample) in self.sample_iter.by_ref() {
            if let Some(output) = self.decimator.decimate_sample(sample) {
                return Some((index, output));
            }
        }
        None
    }
}

impl Decimator {
    pub fn reset(&mut self) {
        *self = Self::new(self.decimation_factor)
    }
    pub fn new(decimation_factor: u32) -> Self {
        Self {
            decimation_factor,
            decimator_count: 0,
        }
    }

    #[inline]
    pub fn decimation_factor(&self) -> u32 {
        self.decimation_factor
    }

    pub fn decimate_sample(&mut self, sample: f32) -> Option<f32> {
        self.decimator_count += 1;
        if self.decimator_count < self.decimation_factor {
            None
        } else {
            self.decimator_count = 0;
            Some(sample)
        }
    }

    /// Creates an iterator which decimates the input samples and yields pairs (index, output_sample),
    /// where i is the index where the output was found.
    pub fn decimate<I: IntoIterator<Item = f32>>(&mut self, samples: I) -> DecimIter<'_, I> {
        let sample_iter = samples.into_iter().enumerate();
        DecimIter {
            sample_iter,
            decimator: self,
        }
    }
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_allpass() {
        let mut dec = Decimator::new(1);
        let decimated: Vec<(usize, f32)> = dec.decimate([3.0, 1.1, 4.0, -1.5, 9.2]).collect();
        assert_eq!(
            decimated,
            &[(0, 3.0), (1, 1.1), (2, 4.0), (3, -1.5), (4, 9.2),]
        );
    }

    #[test]
    fn test_div2() {
        let mut dec = Decimator::new(2);
        let decimated: Vec<(usize, f32)> = dec.decimate([3.0, 1.1, 4.0, -1.5, 9.2]).collect();
        assert_eq!(decimated, &[(1, 1.1), (3, -1.5),]);
    }

    #[test]
    fn test_is_stateful() {
        let mut dec = Decimator::new(7);

        // 50 samples decimated by 7: expect 7th, 14th, 21st, etc sample as output
        let batch1 = (1..=25).into_iter().map(|int| int as f32);
        let batch2 = (26..=50).into_iter().map(|int| int as f32);

        // step1: decimate batch 1
        let decimated: Vec<(usize, f32)> = dec.decimate(batch1).collect();
        assert_eq!(decimated, &[(6, 7.0), (13, 14.0), (20, 21.0),]);

        // step 2: continue with batch 2: expect sample no. 28, 35, 42, 49 with offsets relative to this batch
        let decimated: Vec<(usize, f32)> = dec.decimate(batch2).collect();
        assert_eq!(decimated, &[(2, 28.0), (9, 35.0), (16, 42.0), (23, 49.0),]);
    }
}
