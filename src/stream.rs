//! In-memory compression/decompression streams

use std::error;
use std::fmt;
use std::io;
use std::mem;
use std::slice;

use brotli_sys;
use libc::c_int;

/// In-memory state for decompressing brotli-encoded data.
///
/// This stream is at the heart of the I/O streams and is used to decompress an
/// incoming brotli stream.
pub struct Decompress {
    state: *mut brotli_sys::BrotliDecoderState,
}

unsafe impl Send for Decompress {}
unsafe impl Sync for Decompress {}

/// In-memory state for compressing/encoding data with brotli
///
/// This stream is at the heart of the I/O encoders and is used to compress
/// data.
pub struct Compress {
    state: *mut brotli_sys::BrotliEncoderState,
}

unsafe impl Send for Compress {}
unsafe impl Sync for Compress {}

/// Parameters passed to various compression routines.
#[derive(Clone,Debug)]
pub struct CompressParams {
    /// Compression mode.
    mode: u32,
    /// Controls the compression-speed vs compression-density tradeoffs. The higher the `quality`,
    /// the slower the compression. Range is 0 to 11.
    quality: u32,
    /// Base 2 logarithm of the sliding window size. Range is 10 to 24.
    lgwin: u32,
    /// Base 2 logarithm of the maximum input block size. Range is 16 to 24. If set to 0, the value
    /// will be set based on the quality.
    lgblock: u32,
}

/// Possible choices for modes of compression
#[repr(isize)]
#[derive(Copy,Clone,Debug,PartialEq,Eq)]
pub enum CompressMode {
    /// Default compression mode, the compressor does not know anything in
    /// advance about the properties of the input.
    Generic = brotli_sys::BROTLI_MODE_GENERIC as isize,
    /// Compression mode for utf-8 formatted text input.
    Text = brotli_sys::BROTLI_MODE_TEXT as isize,
    /// Compression mode in WOFF 2.0.
    Font = brotli_sys::BROTLI_MODE_FONT as isize,
}

/// Possible choices for the operation performed by the compressor.
///
/// When using any operation except `Process`, you must *not* alter the
/// input buffer or use a different operation until the current operation
/// has 'completed'. An operation may need to be repeated with more space to
/// write data until it can complete.
#[repr(isize)]
#[derive(Copy,Clone,Debug,PartialEq,Eq)]
pub enum CompressOp {
    /// Compress input data
    Process = brotli_sys::BROTLI_OPERATION_PROCESS as isize,
    /// Compress input data, ensuring that all input so far has been
    /// written out
    Flush = brotli_sys::BROTLI_OPERATION_FLUSH as isize,
    /// Compress input data, ensuring that all input so far has been
    /// written out and then finalizing the stream so no more data can
    /// be written
    Finish = brotli_sys::BROTLI_OPERATION_FINISH as isize,
    /// Emit a metadata block to the stream, an opaque piece of out-of-band
    /// data that does not interfere with the main stream of data. Metadata
    /// blocks *must* be no longer than 16MiB
    EmitMetadata = brotli_sys::BROTLI_OPERATION_EMIT_METADATA as isize,
}

/// Error that can happen from decompressing or compressing a brotli stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Error(());

/// Indication of whether a compression operation is 'complete'. This does
/// not indicate whether the whole stream is complete - see `Compress::compress`
/// for details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoStatus {
    /// The operation completed successfully
    Finished,
    /// The operation has more work to do and needs to be called again with the
    /// same buffer
    Unfinished,
}

/// Possible status results returned from decompressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeStatus {
    /// Decompression was successful and has finished
    Finished,
    /// More input is needed to continue
    NeedInput,
    /// More output is needed to continue
    NeedOutput,
}

impl Decompress {
    /// Creates a new brotli decompression/decoding stream ready to receive
    /// data.
    pub fn new() -> Decompress {
        unsafe {
            let state = brotli_sys::BrotliDecoderCreateInstance(None, None, 0 as *mut _);
            assert!(!state.is_null());
            Decompress { state: state }
        }
    }

    /// Decompress some input data and write it to a buffer of output data.
    ///
    /// This function will decompress the data in `input` and place the output
    /// in `output`, returning the result. Possible statuses that can be
    /// returned are that the stream is finished, more input is needed, or more
    /// output space is needed.
    ///
    /// The `input` slice is updated to point to the remaining data that was not
    /// consumed, and the `output` slice is updated to point to the portion of
    /// the output slice that still needs to be filled in.
    ///
    /// # Errors
    ///
    /// If the input stream is not a valid brotli stream, then an error is
    /// returned.
    pub fn decompress(&mut self,
                      input: &mut &[u8],
                      output: &mut &mut [u8]) -> Result<DeStatus, Error> {
        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total_out = 0;
        let r = unsafe {
            brotli_sys::BrotliDecoderDecompressStream(self.state,
                                                      &mut available_in,
                                                      &mut next_in,
                                                      &mut available_out,
                                                      &mut next_out,
                                                      &mut total_out)
        };
        *input = &input[input.len() - available_in..];
        let out_len = output.len();
        *output = &mut mem::replace(output, &mut [])[out_len - available_out..];
        Decompress::rc(r)
    }

    /// Retrieve a slice of the internal decompressor buffer up to `size_limit` in length
    /// (unlimited length if `None`), consuming it. As the internal buffer may not be
    /// contiguous, consecutive calls may return more output until this function returns
    /// `None`.
    pub fn take_output(&mut self, size_limit: Option<usize>) -> Option<&[u8]> {
        if let Some(0) = size_limit { return None }
        let mut size_limit = size_limit.unwrap_or(0); // 0 now means unlimited
        unsafe {
            let ptr = brotli_sys::BrotliDecoderTakeOutput(self.state, &mut size_limit);
            if size_limit == 0 { // ptr may or may not be null
                None
            } else {
                assert!(!ptr.is_null());
                Some(slice::from_raw_parts(ptr, size_limit))
            }
        }
    }

    fn rc(rc: brotli_sys::BrotliDecoderResult) -> Result<DeStatus, Error> {
        match rc {
            // TODO: get info from BrotliDecoderGetErrorCode/BrotliDecoderErrorString
            // for these decode errors
            brotli_sys::BROTLI_DECODER_RESULT_ERROR => Err(Error(())),
            brotli_sys::BROTLI_DECODER_RESULT_SUCCESS => Ok(DeStatus::Finished),
            brotli_sys::BROTLI_DECODER_RESULT_NEEDS_MORE_INPUT => Ok(DeStatus::NeedInput),
            brotli_sys::BROTLI_DECODER_RESULT_NEEDS_MORE_OUTPUT => Ok(DeStatus::NeedOutput),
            n => panic!("unknown return code: {}", n)
        }
    }
}

impl Drop for Decompress {
    fn drop(&mut self) {
        unsafe {
            brotli_sys::BrotliDecoderDestroyInstance(self.state);
        }
    }
}

/// Decompress data in one go in memory.
///
/// Decompresses the data in `input` into the `output` buffer. The `output`
/// buffer is updated to point to the actual output slice if successful.
pub fn decompress_buf(input: &[u8],
                      output: &mut &mut [u8]) -> Result<usize, Error> {
    let mut size = output.len();
    let r = unsafe {
        brotli_sys::BrotliDecoderDecompress(input.len(),
                                            input.as_ptr(),
                                            &mut size,
                                            output.as_mut_ptr())
    };
    *output = &mut mem::replace(output, &mut [])[..size];
    if r == 0 {
        Err(Error(()))
    } else {
        Ok(size)
    }
}

impl Compress {
    /// Creates a new compressor ready to encode data into brotli
    pub fn new() -> Compress {
        unsafe {
            let state = brotli_sys::BrotliEncoderCreateInstance(None, None, 0 as *mut _);
            assert!(!state.is_null());

            Compress { state: state }
        }
    }

    // TODO: add the BrotliEncoderOperation variants of
    // BrotliEncoderCompressStream here

    /// Pass some input data to the compressor and write it to a buffer of
    /// output data, compressing or otherwise handling it as instructed by
    /// the specified operation.
    ///
    /// This function will handle the data in `input` and place the output
    /// in `output`, returning the Result. Possible statuses are that the
    /// operation is complete or incomplete.
    ///
    /// The `input` slice is updated to point to the remaining data that was not
    /// consumed, and the `output` slice is updated to point to the portion of
    /// the output slice that still needs to be filled in.
    ///
    /// If the result of a compress operation is `Unfinished` (which it may be
    /// for any operation except `Process`), you *must* call the operation again
    /// with the same operation and input buffer and more space to output to.
    /// `Process` will never return `Unfinished`, but it is a logic error to end
    /// a buffer without calling either `Flush` or `Finish` as some output data
    /// may not have been written.
    ///
    /// # Errors
    ///
    /// Returns an error if brotli encountered an error while processing the stream.
    ///
    /// # Examples
    ///
    /// ```
    /// use brotli2::stream::{Error, Compress, CompressOp, CoStatus, decompress_buf};
    ///
    /// // An example of compressing `input` into the destination vector
    /// // `output`, expanding as necessary
    /// fn compress_vec(mut input: &[u8],
    ///                 output: &mut Vec<u8>) -> Result<(), Error> {
    ///     let mut compress = Compress::new();
    ///     let nilbuf = &mut &mut [][..];
    ///     loop {
    ///         // Compressing to a buffer is easiest when the slice is already
    ///         // available - since we need to grow, extend from compressor
    ///         // internal buffer.
    ///         let status = try!(compress.compress(CompressOp::Finish, &mut input, nilbuf));
    ///         while let Some(buf) = compress.take_output(None) {
    ///             output.extend_from_slice(buf)
    ///         }
    ///         match status {
    ///             CoStatus::Finished => break,
    ///             CoStatus::Unfinished => (),
    ///         }
    ///     }
    ///     Ok(())
    /// }
    ///
    /// fn assert_roundtrip(data: &[u8]) {
    ///     let mut compressed = Vec::new();
    ///     compress_vec(data, &mut compressed).unwrap();
    ///
    ///     let mut decompressed = [0; 2048];
    ///     let mut decompressed = &mut decompressed[..];
    ///     decompress_buf(&compressed, &mut decompressed).unwrap();
    ///     assert_eq!(decompressed, data);
    /// }
    ///
    /// assert_roundtrip(b"Hello, World!");
    /// assert_roundtrip(b"");
    /// assert_roundtrip(&[6; 1024]);
    /// ```
    pub fn compress(&mut self,
                    op: CompressOp,
                    input: &mut &[u8],
                    output: &mut &mut [u8]) -> Result<CoStatus, Error> {
        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total_out = 0;
        let r = unsafe {
            brotli_sys::BrotliEncoderCompressStream(self.state,
                                                    op as brotli_sys::BrotliEncoderOperation,
                                                    &mut available_in,
                                                    &mut next_in,
                                                    &mut available_out,
                                                    &mut next_out,
                                                    &mut total_out)
        };
        *input = &input[input.len() - available_in..];
        let out_len = output.len();
        *output = &mut mem::replace(output, &mut [])[out_len - available_out..];
        if r == 0 { return Err(Error(())) }
        Ok(if op == CompressOp::Process {
            CoStatus::Finished
        } else if available_in != 0 {
            CoStatus::Unfinished
        } else if unsafe { brotli_sys::BrotliEncoderHasMoreOutput(self.state) } == 1 {
            CoStatus::Unfinished
        } else if op == CompressOp::Finish &&
                unsafe { brotli_sys::BrotliEncoderIsFinished(self.state) } == 0 {
            CoStatus::Unfinished
        } else {
            CoStatus::Finished
        })
    }

    /// Retrieve a slice of the internal compressor buffer up to `size_limit` in length
    /// (unlimited length if `None`), consuming it. As the internal buffer may not be
    /// contiguous, consecutive calls may return more output until this function returns
    /// `None`.
    pub fn take_output(&mut self, size_limit: Option<usize>) -> Option<&[u8]> {
        if let Some(0) = size_limit { return None }
        let mut size_limit = size_limit.unwrap_or(0); // 0 now means unlimited
        unsafe {
            let ptr = brotli_sys::BrotliEncoderTakeOutput(self.state, &mut size_limit);
            if size_limit == 0 { // ptr may or may not be null
                None
            } else {
                assert!(!ptr.is_null());
                Some(slice::from_raw_parts(ptr, size_limit))
            }
        }
    }

    /// Configure the parameters of this compression session.
    ///
    /// Note that this is likely to only successful if called before compression
    /// starts.
    pub fn set_params(&mut self, params: &CompressParams) {
        unsafe {
            brotli_sys::BrotliEncoderSetParameter(self.state,
                                                  brotli_sys::BROTLI_PARAM_MODE,
                                                  params.mode);
            brotli_sys::BrotliEncoderSetParameter(self.state,
                                                  brotli_sys::BROTLI_PARAM_QUALITY,
                                                  params.quality);
            brotli_sys::BrotliEncoderSetParameter(self.state,
                                                  brotli_sys::BROTLI_PARAM_LGWIN,
                                                  params.lgwin);
            brotli_sys::BrotliEncoderSetParameter(self.state,
                                                  brotli_sys::BROTLI_PARAM_LGBLOCK,
                                                  params.lgblock);
            // TODO: add these two
            // brotli_sys::BrotliEncoderSetParameter(self.state,
            //                                       brotli_sys::BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING,
            //                                       params.lgblock);
            // brotli_sys::BrotliEncoderSetParameter(self.state,
            //                                       brotli_sys::BROTLI_PARAM_SIZE_HINT,
            //                                       params.lgblock);
        }
    }
}

impl Drop for Compress {
    fn drop(&mut self) {
        unsafe {
            brotli_sys::BrotliEncoderDestroyInstance(self.state);
        }
    }
}

/// Compresses the data in `input` into `output`.
///
/// The `output` buffer is updated to point to the exact slice which contains
/// the output data.
///
/// If successful, the amount of compressed bytes are returned (the size of the
/// `output` slice), and otherwise an error is returned.
pub fn compress_buf(params: &CompressParams,
                    input: &[u8],
                    output: &mut &mut [u8]) -> Result<usize, Error> {
    let mut size = output.len();
    let r = unsafe {
        brotli_sys::BrotliEncoderCompress(params.quality as c_int,
                                          params.lgwin as c_int,
                                          params.mode as brotli_sys::BrotliEncoderMode,
                                          input.len(),
                                          input.as_ptr(),
                                          &mut size,
                                          output.as_mut_ptr())
    };
    *output = &mut mem::replace(output, &mut [])[..size];
    if r == 0 {
        Err(Error(()))
    } else {
        Ok(size)
    }
}

impl CompressParams {
    /// Creates a new default set of compression parameters.
    pub fn new() -> CompressParams {
        CompressParams {
            mode: brotli_sys::BROTLI_DEFAULT_MODE,
            quality: brotli_sys::BROTLI_DEFAULT_QUALITY,
            lgwin: brotli_sys::BROTLI_DEFAULT_WINDOW,
            lgblock: 0,
        }
    }

    /// Set the mode of this compression.
    pub fn mode(&mut self, mode: CompressMode) -> &mut CompressParams {
        self.mode = mode as u32;
        self
    }

    /// Controls the compression-speed vs compression-density tradeoffs.
    ///
    /// The higher the quality, the slower the compression. Currently the range
    /// for the quality is 0 to 11.
    pub fn quality(&mut self, quality: u32) -> &mut CompressParams {
        self.quality = quality;
        self
    }

    /// Sets the base 2 logarithm of the sliding window size.
    ///
    /// Currently the range is 10 to 24.
    pub fn lgwin(&mut self, lgwin: u32) -> &mut CompressParams {
        self.lgwin = lgwin;
        self
    }

    /// Sets the base 2 logarithm of the maximum input block size.
    ///
    /// Currently the range is 16 to 24, and if set to 0 the value will be set
    /// based on the quality.
    pub fn lgblock(&mut self, lgblock: u32) -> &mut CompressParams {
        self.lgblock = lgblock;
        self
    }

    /// Get the current block size
    #[inline]
    pub fn get_lgblock_readable(&self) -> usize {
        1usize << self.lgblock
    }

    /// Get the native lgblock size
    #[inline]
    pub fn get_lgblock(&self) -> u32 {
        self.lgblock.clone()
    }
    /// Get the current window size
    #[inline]
    pub fn get_lgwin_readable(&self) -> usize {
        1usize << self.lgwin
    }
    /// Get the native lgwin value
    #[inline]
    pub fn get_lgwin(&self) -> u32 {
        self.lgwin.clone()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        error::Error::description(self).fmt(f)
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        "brotli error"
    }
}

impl From<Error> for io::Error {
    fn from(_err: Error) -> io::Error {
        io::Error::new(io::ErrorKind::Other, "brotli error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompress_error() {
        let mut d = Decompress::new();
        d.decompress(&mut &[0; 1024][..], &mut &mut [0; 2048][..]).unwrap_err();
    }

    #[test]
    fn compress_buf_smoke() {
        let mut data = [0; 128];
        let mut data = &mut data[..];
        compress_buf(&CompressParams::new(), b"hello!", &mut data).unwrap();

        let mut dst = [0; 128];
        {
            let mut dst = &mut dst[..];
            let n = decompress_buf(data, &mut dst).unwrap();
            assert_eq!(n, dst.len());
            assert_eq!(dst.len(), 6);
        }
        assert_eq!(&dst[..6], b"hello!");
    }

    #[test]
    fn decompress_smoke() {
        let mut data = [0; 128];
        let mut data = &mut data[..];
        compress_buf(&CompressParams::new(), b"hello!", &mut data).unwrap();

        let mut d = Decompress::new();
        let mut dst = [0; 128];
        {
            let mut data = &data[..];
            let mut dst = &mut dst[..];
            assert_eq!(d.decompress(&mut data, &mut dst), Ok(DeStatus::Finished));
        }
        assert_eq!(&dst[..6], b"hello!");
    }

    #[test]
    fn compress_smoke() {
        let mut data = [0; 128];
        let mut dst = [0; 128];

        {
            let mut data = &mut data[..];
            let mut c = Compress::new();
            let mut input = &mut &b"hello!"[..];
            assert_eq!(c.compress(CompressOp::Finish, input, &mut data), Ok(CoStatus::Finished));
            assert!(input.is_empty());
        }
        decompress_buf(&data, &mut &mut dst[..]).unwrap();
        assert_eq!(&dst[..6], b"hello!");

        {
            let mut data = &mut data[..];
            let mut c = Compress::new();
            let mut input = &mut &b"hel"[..];
            assert_eq!(c.compress(CompressOp::Flush, input, &mut data), Ok(CoStatus::Finished));
            assert!(input.is_empty());
            let mut input = &mut &b"lo!"[..];
            assert_eq!(c.compress(CompressOp::Finish, input, &mut data), Ok(CoStatus::Finished));
            assert!(input.is_empty());
        }
        decompress_buf(&data, &mut &mut dst[..]).unwrap();
        assert_eq!(&dst[..6], b"hello!");
    }
}
