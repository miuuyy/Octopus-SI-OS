import struct


_MASK = 0xFFFFFFFF
_IV = (
    0x6A09E667,
    0xBB67AE85,
    0x3C6EF372,
    0xA54FF53A,
    0x510E527F,
    0x9B05688C,
    0x1F83D9AB,
    0x5BE0CD19,
)
_PERMUTATION = (2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)
_CHUNK_START = 1
_CHUNK_END = 2
_PARENT = 4
_ROOT = 8
_BLOCK_LEN = 64
_CHUNK_LEN = 1024


def _rotate_right(value, count):
    return ((value >> count) | (value << (32 - count))) & _MASK


def _mix(state, a, b, c, d, x, y):
    state[a] = (state[a] + state[b] + x) & _MASK
    state[d] = _rotate_right(state[d] ^ state[a], 16)
    state[c] = (state[c] + state[d]) & _MASK
    state[b] = _rotate_right(state[b] ^ state[c], 12)
    state[a] = (state[a] + state[b] + y) & _MASK
    state[d] = _rotate_right(state[d] ^ state[a], 8)
    state[c] = (state[c] + state[d]) & _MASK
    state[b] = _rotate_right(state[b] ^ state[c], 7)


def _round(state, message):
    _mix(state, 0, 4, 8, 12, message[0], message[1])
    _mix(state, 1, 5, 9, 13, message[2], message[3])
    _mix(state, 2, 6, 10, 14, message[4], message[5])
    _mix(state, 3, 7, 11, 15, message[6], message[7])
    _mix(state, 0, 5, 10, 15, message[8], message[9])
    _mix(state, 1, 6, 11, 12, message[10], message[11])
    _mix(state, 2, 7, 8, 13, message[12], message[13])
    _mix(state, 3, 4, 9, 14, message[14], message[15])


def _compress(chaining_value, block_words, counter, block_len, flags):
    state = list(chaining_value) + list(_IV[:4]) + [
        counter & _MASK,
        (counter >> 32) & _MASK,
        block_len,
        flags,
    ]
    message = list(block_words)
    for round_index in range(7):
        _round(state, message)
        if round_index != 6:
            message = [message[index] for index in _PERMUTATION]
    return tuple(state[index] ^ state[index + 8] for index in range(8)) + tuple(
        state[index + 8] ^ chaining_value[index] for index in range(8)
    )


def _block_words(value):
    if len(value) > _BLOCK_LEN:
        raise ValueError("Octopus content-hash block exceeds 64 bytes")
    return struct.unpack("<16I", value.ljust(_BLOCK_LEN, b"\0"))


class _Output:
    def __init__(self, input_chaining_value, block_words, counter, block_len, flags):
        self.input_chaining_value = input_chaining_value
        self.block_words = block_words
        self.counter = counter
        self.block_len = block_len
        self.flags = flags

    def chaining_value(self):
        return _compress(
            self.input_chaining_value,
            self.block_words,
            self.counter,
            self.block_len,
            self.flags,
        )[:8]

    def root_hash(self):
        words = _compress(
            self.input_chaining_value,
            self.block_words,
            0,
            self.block_len,
            self.flags | _ROOT,
        )
        return struct.pack("<16I", *words)[:32]


class _ChunkState:
    def __init__(self, chunk_counter):
        self.chaining_value = _IV
        self.chunk_counter = chunk_counter
        self.block = bytearray()
        self.blocks_compressed = 0

    def length(self):
        return self.blocks_compressed * _BLOCK_LEN + len(self.block)

    def start_flag(self):
        return _CHUNK_START if self.blocks_compressed == 0 else 0

    def update(self, value):
        view = memoryview(value)
        offset = 0
        while offset < len(view):
            if len(self.block) == _BLOCK_LEN:
                self.chaining_value = _compress(
                    self.chaining_value,
                    _block_words(bytes(self.block)),
                    self.chunk_counter,
                    _BLOCK_LEN,
                    self.start_flag(),
                )[:8]
                self.blocks_compressed += 1
                self.block.clear()
            take = min(_BLOCK_LEN - len(self.block), len(view) - offset)
            self.block.extend(view[offset : offset + take])
            offset += take

    def output(self):
        return _Output(
            self.chaining_value,
            _block_words(bytes(self.block)),
            self.chunk_counter,
            len(self.block),
            self.start_flag() | _CHUNK_END,
        )


def _parent_output(left, right):
    return _Output(_IV, tuple(left) + tuple(right), 0, _BLOCK_LEN, _PARENT)


class _Hasher:
    def __init__(self):
        self.chunk_state = _ChunkState(0)
        self.chaining_values = []

    def _add_chunk(self, chaining_value, total_chunks):
        while total_chunks & 1 == 0:
            chaining_value = _parent_output(
                self.chaining_values.pop(), chaining_value
            ).chaining_value()
            total_chunks >>= 1
        self.chaining_values.append(chaining_value)

    def update(self, value):
        if not isinstance(value, (bytes, bytearray, memoryview)):
            raise TypeError("Octopus content hash input must be bytes")
        view = memoryview(value)
        offset = 0
        while offset < len(view):
            if self.chunk_state.length() == _CHUNK_LEN:
                chunk_value = self.chunk_state.output().chaining_value()
                total_chunks = self.chunk_state.chunk_counter + 1
                self._add_chunk(chunk_value, total_chunks)
                self.chunk_state = _ChunkState(total_chunks)
            take = min(_CHUNK_LEN - self.chunk_state.length(), len(view) - offset)
            self.chunk_state.update(view[offset : offset + take])
            offset += take
        return self

    def digest(self):
        output = self.chunk_state.output()
        for left in reversed(self.chaining_values):
            output = _parent_output(left, output.chaining_value())
        return output.root_hash()


def octopus_content_hash(value):
    return _Hasher().update(value).digest().hex()
