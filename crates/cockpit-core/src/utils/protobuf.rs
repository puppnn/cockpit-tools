/// Protobuf Varint 编码
pub fn encode_varint(mut value: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    while value >= 0x80 {
        buf.push((value & 0x7F | 0x80) as u8);
        value >>= 7;
    }
    buf.push(value as u8);
    buf
}

/// 编码长度分隔字段 (wire_type = 2)
pub fn encode_len_delim_field(field_num: u32, data: &[u8]) -> Vec<u8> {
    let tag = (field_num << 3) | 2;
    let mut f = encode_varint(tag as u64);
    f.extend(encode_varint(data.len() as u64));
    f.extend_from_slice(data);
    f
}

/// 编码字符串字段 (wire_type = 2)
pub fn encode_string_field(field_num: u32, value: &str) -> Vec<u8> {
    encode_len_delim_field(field_num, value.as_bytes())
}

/// 读取 Protobuf Varint
pub fn read_varint(data: &[u8], offset: usize) -> Result<(u64, usize), String> {
    let mut result = 0u64;
    let mut shift = 0;
    let mut pos = offset;

    loop {
        if pos >= data.len() {
            return Err("数据不完整".to_string());
        }
        let byte = data[pos];
        result |= ((byte & 0x7F) as u64) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }

    Ok((result, pos))
}

/// 跳过 Protobuf 字段
pub fn skip_field(data: &[u8], offset: usize, wire_type: u8) -> Result<usize, String> {
    match wire_type {
        0 => {
            // Varint
            let (_, new_offset) = read_varint(data, offset)?;
            Ok(new_offset)
        }
        1 => {
            // 64-bit
            Ok(offset + 8)
        }
        2 => {
            // Length-delimited
            let (length, content_offset) = read_varint(data, offset)?;
            Ok(content_offset + length as usize)
        }
        5 => {
            // 32-bit
            Ok(offset + 4)
        }
        _ => Err(format!("未知 wire_type: {}", wire_type)),
    }
}

/// 移除指定的 Protobuf 字段
pub fn remove_field(data: &[u8], field_num: u32) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        let start_offset = offset;
        let (tag, new_offset) = read_varint(data, offset)?;
        let wire_type = (tag & 7) as u8;
        let current_field = (tag >> 3) as u32;

        if current_field == field_num {
            // 跳过此字段
            offset = skip_field(data, new_offset, wire_type)?;
        } else {
            // 保留其他字段
            let next_offset = skip_field(data, new_offset, wire_type)?;
            result.extend_from_slice(&data[start_offset..next_offset]);
            offset = next_offset;
        }
    }

    Ok(result)
}

/// 创建 OAuthTokenInfo 消息
pub fn create_oauth_info(access_token: &str, refresh_token: &str, expiry: i64) -> Vec<u8> {
    // Field 1: access_token (string, wire_type = 2)
    let field1 = encode_string_field(1, access_token);

    // Field 2: token_type (string, fixed value "Bearer", wire_type = 2)
    let field2 = encode_string_field(2, "Bearer");

    // Field 3: refresh_token (string, wire_type = 2)
    let field3 = encode_string_field(3, refresh_token);

    // Field 4: expiry (嵌套的 Timestamp 消息, wire_type = 2)
    let timestamp_tag = (1 << 3) | 0;
    let mut timestamp_msg = encode_varint(timestamp_tag);
    timestamp_msg.extend(encode_varint(expiry as u64));

    let field4 = encode_len_delim_field(4, &timestamp_msg);

    // 合并所有字段为 OAuthTokenInfo 消息
    [field1, field2, field3, field4].concat()
}

/// 创建 OAuthTokenInfo (Field 6)
pub fn create_oauth_field(access_token: &str, refresh_token: &str, expiry: i64) -> Vec<u8> {
    let oauth_info = create_oauth_info(access_token, refresh_token, expiry);
    encode_len_delim_field(6, &oauth_info)
}

/// 从 protobuf 数据中提取 refresh_token
/// 结构: Field 6 (OAuthTokenInfo) -> Field 3 (refresh_token)
pub fn extract_refresh_token(data: &[u8]) -> Option<String> {
    // 先找到 Field 6
    let mut offset = 0;
    while offset < data.len() {
        let (tag, new_offset) = read_varint(data, offset).ok()?;
        let wire_type = (tag & 7) as u8;
        let field_num = (tag >> 3) as u32;

        if field_num == 6 && wire_type == 2 {
            // 找到 Field 6，读取其长度和内容
            let (length, content_offset) = read_varint(data, new_offset).ok()?;
            let length = length as usize;
            if content_offset + length > data.len() {
                return None;
            }
            let oauth_data = &data[content_offset..content_offset + length];

            // 在 OAuthTokenInfo 中找 Field 3 (refresh_token)
            return extract_string_field(oauth_data, 3);
        }

        offset = skip_field(data, new_offset, wire_type).ok()?;
    }

    None
}

/// 从 protobuf 消息中提取指定字段的字符串
fn extract_string_field(data: &[u8], target_field: u32) -> Option<String> {
    let mut offset = 0;
    while offset < data.len() {
        let (tag, new_offset) = read_varint(data, offset).ok()?;
        let wire_type = (tag & 7) as u8;
        let field_num = (tag >> 3) as u32;

        if field_num == target_field && wire_type == 2 {
            let (length, content_offset) = read_varint(data, new_offset).ok()?;
            let length = length as usize;
            if content_offset + length > data.len() {
                return None;
            }
            let value = &data[content_offset..content_offset + length];
            return String::from_utf8(value.to_vec()).ok();
        }

        offset = skip_field(data, new_offset, wire_type).ok()?;
    }

    None
}
