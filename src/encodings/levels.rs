// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use std::cmp;
use std::mem;

use basic::Encoding;
use data_type::AsBytes;
use errors::{Result, ParquetError};
use util::bit_util::{BitReader, BitWriter, ceil, log2};
use util::memory::ByteBufferPtr;
use super::rle_encoding::{RleEncoder, RleDecoder};

enum InternalEncoder {
  RLE(RleEncoder),
  BIT_PACKED(BitWriter),
}

enum InternalDecoder {
  RLE(RleDecoder),
  BIT_PACKED(BitReader),
}

/// A encoder for definition/repetition levels.
/// Currently only supports RLE and BIT_PACKED (dev/null) encoding.
pub struct LevelEncoder {
  bit_width: u8,
  encoder: InternalEncoder,
}

impl LevelEncoder {
  /// Creates new level encoder based on encoding, max level and underlying byte buffer.
  /// For bit packed encoding it is assumed that buffer is already allocated with
  /// 'LevelEncoder::max_buffer_size' method.
  ///
  /// Panics, if encoding is not supported
  pub fn new(encoding: Encoding, max_level: i16, byte_buffer: Vec<u8>) -> Self {
    let bit_width = log2(max_level as u64 + 1) as u8;
    match encoding {
      Encoding::RLE => {
        LevelEncoder {
          bit_width: bit_width,
          encoder: InternalEncoder::RLE(
            RleEncoder::new_from_buf(bit_width, byte_buffer, mem::size_of::<i32>()))
        }
      },
      Encoding::BIT_PACKED => {
        // Here we set full byte buffer without adjusting for num_buffered_values,
        // because byte buffer will already be allocated with size from
        // `max_buffer_size()` method.
        LevelEncoder {
          bit_width: bit_width,
          encoder: InternalEncoder::BIT_PACKED(BitWriter::new_from_buf(byte_buffer, 0))
        }
      },
      _ => panic!("Unsupported encoding type {}", encoding)
    }
  }

  /// Put/encode levels vector into this level encoder.
  /// Returns number of encoded values that are less than or equal to length of the input
  /// buffer.
  ///
  /// RLE and BIT_PACKED level encoders return Err() when internal buffer overflows or
  /// flush fails.
  #[inline]
  pub fn put(&mut self, buffer: &[i16]) -> Result<usize> {
    let mut num_encoded = 0;
    match self.encoder {
      InternalEncoder::RLE(ref mut rle_encoder) => {
        for value in buffer {
          if !rle_encoder.put(*value as u64)? { break; }
          num_encoded += 1;
        }
        rle_encoder.flush()?;
      },
      InternalEncoder::BIT_PACKED(ref mut bit_packed_encoder) => {
        for value in buffer {
          if !bit_packed_encoder.put_value(*value as u64, self.bit_width as usize) {
            return Err(general_err!("Not enough bytes left"));
          }
          num_encoded += 1;
        }
        bit_packed_encoder.flush();
      },
    }
    Ok(num_encoded)
  }

  /// Computes max buffer size for level encoder/decoder based on encoding, max
  /// repetition/definition level and number of total buffered values (includes null
  /// values).
  #[inline]
  pub fn max_buffer_size(
    encoding: Encoding, max_level: i16, num_buffered_values: usize
  ) -> usize {
    let bit_width = log2(max_level as u64 + 1) as u8;
    match encoding {
      Encoding::RLE => {
        RleEncoder::max_buffer_size(bit_width, num_buffered_values) +
          RleEncoder::min_buffer_size(bit_width)
      },
      Encoding::BIT_PACKED => {
        ceil((num_buffered_values * bit_width as usize) as i64, 8) as usize
      },
      _ => panic!("Unsupported encoding type {}", encoding)
    }
  }

  /// Finalizes level encoder, flush all intermediate buffers and return resulting
  /// encoded buffer. Returned buffer is already truncated to encoded bytes only.
  #[inline]
  pub fn consume(self) -> Result<Vec<u8>> {
    match self.encoder {
      InternalEncoder::RLE(mut rle_encoder) => {
        rle_encoder.flush()?;
        let len = (rle_encoder.len() as i32).to_le();
        let len_bytes = len.as_bytes();
        let mut encoded_data = rle_encoder.consume();
        encoded_data[0..len_bytes.len()].copy_from_slice(len_bytes);
        Ok(encoded_data)
      },
      InternalEncoder::BIT_PACKED(bit_packed_encoder) => {
        Ok(bit_packed_encoder.consume())
      },
    }
  }
}

/// A decoder for definition/repetition levels.
/// Currently only supports RLE and BIT_PACKED (dev/null) encoding.
pub struct LevelDecoder {
  bit_width: u8,
  num_values: Option<usize>,
  decoder: InternalDecoder
}

impl LevelDecoder {
  /// Creates new level decoder based on encoding and max definition/repetition level.
  /// This method only initializes level decoder, `set_data()` method must be called
  /// before reading any value.
  ///
  /// Panics if encoding is not supported
  pub fn new(encoding: Encoding, max_level: i16) -> Self {
    let bit_width = log2(max_level as u64 + 1) as u8;
    let decoder = match encoding {
      Encoding::RLE => InternalDecoder::RLE(RleDecoder::new(bit_width)),
      Encoding::BIT_PACKED => InternalDecoder::BIT_PACKED(BitReader::from(Vec::new())),
      _ => panic!("Unsupported encoding type {}", encoding),
    };
    LevelDecoder { bit_width: bit_width, num_values: None, decoder: decoder }
  }

  /// Sets data for this level decoder, and returns total number of bytes set.
  ///
  /// `data` is encoded data as byte buffer, `num_buffered_values` represents total number
  /// of values that is expected.
  ///
  /// Both RLE and BIT_PACKED level decoders set `num_buffered_values` as total number of
  /// values that they can return and track num values.
  #[inline]
  pub fn set_data(&mut self, num_buffered_values: usize, data: ByteBufferPtr) -> usize {
    self.num_values = Some(num_buffered_values);
    match self.decoder {
      InternalDecoder::RLE(ref mut rle_decoder) => {
        let i32_size = mem::size_of::<i32>();
        let data_size = read_num_bytes!(i32, i32_size, data.as_ref()) as usize;
        rle_decoder.set_data(data.range(i32_size, data_size));
        i32_size + data_size
      },
      InternalDecoder::BIT_PACKED(ref mut bit_packed_decoder) => {
        // Set appropriate number of bytes: if max size is larger than buffer - set full
        // buffer
        let num_bytes = ceil((num_buffered_values * self.bit_width as usize) as i64, 8);
        let data_size = cmp::min(num_bytes as usize, data.len());
        bit_packed_decoder.reset(data.range(data.start(), data_size));
        data_size
      },
    }
  }

  /// Sets byte array explicitly when start position `start` and length `len` are known in
  /// advance. Only supported by RLE level decoder.
  /// Returns number of total bytes set for this decoder (len)
  #[inline]
  pub fn set_data_range(&mut self, num_buffered_values: usize, data: &ByteBufferPtr,
      start: usize, len: usize) -> usize {
    match self.decoder {
      InternalDecoder::RLE(ref mut rle_decoder) => {
        rle_decoder.set_data(data.range(start, len));
        self.num_values = Some(num_buffered_values);
        len
      },
      _ => panic!("set_data_range() method is only supported by RLE encoding type"),
    }
  }

  /// Decodes values and puts them into `buffer`.
  /// Returns number of values that were successfully decoded (less than or equal to
  /// buffer length).
  #[inline]
  pub fn get(&mut self, buffer: &mut [i16]) -> Result<usize> {
    assert!(self.num_values.is_some(), "No data set for decoding");
    let values_read = match self.decoder {
      InternalDecoder::RLE(ref mut rle_decoder) => {
        // Max length we can read
        let len = cmp::min(self.num_values.unwrap(), buffer.len());
        rle_decoder.get_batch::<i16>(&mut buffer[0..len])?
      },
      InternalDecoder::BIT_PACKED(ref mut bit_packed_decoder) => {
        // When extracting values from bit reader, it might return more values than left
        // because of padding to a full byte, we use num_values to track precise number
        // of values.
        // TODO: Use get_batch() for bit packed decoder
        let mut values_read = 0;
        let len = cmp::min(self.num_values.unwrap(), buffer.len());
        while values_read < len {
          if let Some(value) = bit_packed_decoder.get_value::<i16>(
            self.bit_width as usize) {
            buffer[values_read] = value;
            values_read += 1;
          } else {
            break;
          }
        }
        values_read
      },
    };
    // Update current num_values
    self.num_values = self.num_values.map(|len| len - values_read);
    Ok(values_read)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use util::test_common::random_numbers_range;

  fn test_internal_roundtrip(enc: Encoding, levels: &[i16], max_level: i16) {
    let size = LevelEncoder::max_buffer_size(enc, max_level, levels.len());
    let mut encoder = LevelEncoder::new(enc, max_level, vec![0; size]);
    encoder.put(&levels).expect("put() should be OK");
    let encoded_levels = encoder.consume().expect("consume() should be OK");

    let mut decoder = LevelDecoder::new(enc, max_level);
    decoder.set_data(levels.len(), ByteBufferPtr::new(encoded_levels));
    let mut buffer = vec![0; levels.len()];
    let num_decoded = decoder.get(&mut buffer).expect("get() should be OK");
    assert_eq!(num_decoded, levels.len());
    assert_eq!(buffer, levels);
  }

  // Performs incremental read until all bytes are read
  fn test_internal_roundtrip_incremental(enc: Encoding, levels: &[i16], max_level: i16) {
    let size = LevelEncoder::max_buffer_size(enc, max_level, levels.len());
    let mut encoder = LevelEncoder::new(enc, max_level, vec![0; size]);
    encoder.put(&levels).expect("put() should be OK");
    let encoded_levels = encoder.consume().expect("consume() should be OK");

    let mut decoder = LevelDecoder::new(enc, max_level);
    decoder.set_data(levels.len(), ByteBufferPtr::new(encoded_levels));

    let mut buffer = vec![0; levels.len() * 2];
    let mut total_decoded = 0;
    let mut safe_stop = levels.len() * 2; // still terminate in case of issues in the code
    while safe_stop > 0 {
      safe_stop -= 1;
      let num_decoded = decoder.get(&mut buffer[total_decoded..total_decoded + 1])
        .expect("get() should be OK");
      if num_decoded == 0 {
        break;
      }
      total_decoded += num_decoded;
    }
    assert!(safe_stop > 0, "Failed to read values incrementally, reached safe stop");
    assert_eq!(total_decoded, levels.len());
    assert_eq!(&buffer[0..levels.len()], levels);
  }

  // Tests encoding/decoding of values when output buffer is larger than number of
  // encoded values
  fn test_internal_roundtrip_underflow(enc: Encoding, levels: &[i16], max_level: i16) {
    let size = LevelEncoder::max_buffer_size(enc, max_level, levels.len());
    let mut encoder = LevelEncoder::new(enc, max_level, vec![0; size]);
    // Encode only one value
    let num_encoded = encoder.put(&levels[0..1]).expect("put() should be OK");
    let encoded_levels = encoder.consume().expect("consume() should be OK");
    assert_eq!(num_encoded, 1);

    let mut decoder = LevelDecoder::new(enc, max_level);
    // Set one encoded value as `num_buffered_values`
    decoder.set_data(1, ByteBufferPtr::new(encoded_levels));
    let mut buffer = vec![0; levels.len()];
    let num_decoded = decoder.get(&mut buffer).expect("get() should be OK");
    assert_eq!(num_decoded, num_encoded);
    assert_eq!(buffer[0..num_decoded], levels[0..num_decoded]);
  }

  // Tests when encoded values are larger than encoder's buffer
  fn test_internal_roundtrip_overflow(enc: Encoding, levels: &[i16], max_level: i16) {
    let size = LevelEncoder::max_buffer_size(enc, max_level, levels.len());
    let mut encoder = LevelEncoder::new(enc, max_level, vec![0; size]);
    let mut found_err = false;
    // Insert a large number of values, so we run out of space
    for _ in 0..100 {
      match encoder.put(&levels) {
        Err(err) => {
          assert!(format!("{}", err).contains("Not enough bytes left"));
          found_err = true;
          break;
        },
        Ok(_) => { },
      }
    }
    if !found_err {
      panic!("Failed test: no buffer overflow");
    }
  }

  #[test]
  fn test_roundtrip_one() {
    let levels = vec![0, 1, 1, 1, 1, 0, 0, 0, 0, 1];
    let max_level = 1;
    test_internal_roundtrip(Encoding::RLE, &levels, max_level);
    test_internal_roundtrip(Encoding::BIT_PACKED, &levels, max_level);
  }

  #[test]
  fn test_roundtrip() {
    let levels = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let max_level = 10;
    test_internal_roundtrip(Encoding::RLE, &levels, max_level);
    test_internal_roundtrip(Encoding::BIT_PACKED, &levels, max_level);
  }

  #[test]
  fn test_roundtrip_incremental() {
    let levels = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let max_level = 10;
    test_internal_roundtrip_incremental(Encoding::RLE, &levels, max_level);
    test_internal_roundtrip_incremental(Encoding::BIT_PACKED, &levels, max_level);
  }

  #[test]
  fn test_roundtrip_all_zeros() {
    let levels = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let max_level = 1;
    test_internal_roundtrip(Encoding::RLE, &levels, max_level);
    test_internal_roundtrip(Encoding::BIT_PACKED, &levels, max_level);
  }

  #[test]
  fn test_roundtrip_random() {
    // This test is mainly for bit packed level encoder/decoder
    let mut levels = Vec::new();
    let max_level = 5;
    random_numbers_range::<i16>(120, 0, max_level, &mut levels);
    test_internal_roundtrip(Encoding::RLE, &levels, max_level);
    test_internal_roundtrip(Encoding::BIT_PACKED, &levels, max_level);
  }

  #[test]
  fn test_roundtrip_underflow() {
    let levels = vec![1, 1, 2, 3, 2, 1, 1, 2, 3, 1];
    let max_level = 3;
    test_internal_roundtrip_underflow(Encoding::RLE, &levels, max_level);
    test_internal_roundtrip_underflow(Encoding::BIT_PACKED, &levels, max_level);
  }

  #[test]
  fn test_roundtrip_overflow() {
    let levels = vec![1, 1, 2, 3, 2, 1, 1, 2, 3, 1];
    let max_level = 3;
    test_internal_roundtrip_overflow(Encoding::RLE, &levels, max_level);
    test_internal_roundtrip_overflow(Encoding::BIT_PACKED, &levels, max_level);
  }

  #[test]
  fn test_rle_decoder_set_data_range() {
    // Buffer containing both repetition and definition levels
    let buffer = ByteBufferPtr::new(vec![5, 198, 2, 5, 42, 168, 10, 0, 2, 3, 36, 73]);

    let max_rep_level = 1;
    let mut decoder = LevelDecoder::new(Encoding::RLE, max_rep_level);
    assert_eq!(decoder.set_data_range(10, &buffer, 0, 3), 3);
    let mut result = vec![0; 10];
    let num_decoded = decoder.get(&mut result).expect("get() should be OK");
    assert_eq!(num_decoded, 10);
    assert_eq!(result, vec![0, 1, 1, 0, 0, 0, 1, 1, 0, 1]);

    let max_def_level = 2;
    let mut decoder = LevelDecoder::new(Encoding::RLE, max_def_level);
    assert_eq!(decoder.set_data_range(10, &buffer, 3, 5), 5);
    let mut result = vec![0; 10];
    let num_decoded = decoder.get(&mut result).expect("get() should be OK");
    assert_eq!(num_decoded, 10);
    assert_eq!(result, vec![2, 2, 2, 0, 0, 2, 2, 2, 2, 2]);
  }

  #[test]
  #[should_panic(
    expected = "set_data_range() method is only supported by RLE encoding type"
  )]
  fn test_bit_packed_decoder_set_data_range() {
    // Buffer containing both repetition and definition levels
    let buffer = ByteBufferPtr::new(vec![1, 2, 3, 4, 5]);
    let max_level = 1;
    let mut decoder = LevelDecoder::new(Encoding::BIT_PACKED, max_level);
    decoder.set_data_range(10, &buffer, 0, 3);
  }

  #[test]
  fn test_bit_packed_decoder_set_data() {
    // Test the maximum size that is assigned based on number of values and buffer length
    let buffer = ByteBufferPtr::new(vec![1, 2, 3, 4, 5]);
    let max_level = 1;
    let mut decoder = LevelDecoder::new(Encoding::BIT_PACKED, max_level);
    // This should reset to entire buffer
    assert_eq!(decoder.set_data(1024, buffer.all()), buffer.len());
    // This should set smallest num bytes
    assert_eq!(decoder.set_data(3, buffer.all()), 1);
  }

  #[test]
  #[should_panic(expected = "No data set for decoding")]
  fn test_rle_level_decoder_get_no_set_data() {
    // `get()` normally panics because bit_reader is not set for RLE decoding
    // we have explicit check now in set_data
    let max_rep_level = 2;
    let mut decoder = LevelDecoder::new(Encoding::RLE, max_rep_level);
    let mut buffer = vec![0; 16];
    decoder.get(&mut buffer).unwrap();
  }

  #[test]
  #[should_panic(expected = "No data set for decoding")]
  fn test_bit_packed_level_decoder_get_no_set_data() {
    let max_rep_level = 2;
    let mut decoder = LevelDecoder::new(Encoding::BIT_PACKED, max_rep_level);
    let mut buffer = vec![0; 16];
    decoder.get(&mut buffer).unwrap();
  }
}
