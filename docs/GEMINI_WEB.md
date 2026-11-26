# Gemini Web trong Muse Desk

Gemini là backend thứ hai của cùng ứng dụng; không cần chạy server Python/Go,
Chromium thường trực hay một dự án proxy khác. Adapter Rust riêng nằm ở
`src/adapters/gemini`; bootstrap cookie, HTTP StreamGenerate, framing và upload
ảnh độc lập với WebSocket/Noise của Muse.

## Dùng trong app

1. Mở **Tài khoản → Thêm**, chọn **Gemini Web**.
2. Trên trình duyệt của bạn, đăng nhập `https://gemini.google.com/app`.
3. Mở DevTools → Network, tải lại trang, chọn một request tới `gemini.google.com`.
4. Sao chép giá trị header **Cookie** có `__Secure-1PSID`; giữ cả
   `__Secure-1PSIDTS` khi có. Không dùng cookie Muse hoặc OAuth client secret.
5. Dán vào app, đặt tên gợi nhớ; có thể nhập proxy HTTP/HTTPS riêng cho tài khoản.
6. Lưu, chọn tài khoản, bấm **Model / tư duy**. Nhập số tài khoản Google trong URL
   (ví dụ `/u/5/app` → 5), tải danh sách, chọn model/mức tư duy rồi Lưu.
7. Bấm **Chuyển tài khoản/Kết nối lại**, rồi **Mở Claude Code**.

Cookie và proxy được mã hóa Windows DPAPI như tài khoản Muse. Không cần gửi cookie
qua chat. App không tự đọc cookie từ Chrome hay hồ sơ trình duyệt.

Danh sách cũ mặc định vẫn là Muse, không mất tài khoản/cookie/proxy. Một tài khoản
chỉ có một nguồn; để đổi nguồn hãy thêm tài khoản mới. Mỗi lúc chỉ có một backend
hoạt động. Khi đổi tài khoản, proxy giữ nguyên API key và khởi động lại bằng
cookie/proxy của tài khoản mới. Mở phiên Claude mới sau khi chuyển.
Không dùng `--resume` hội thoại Muse để chuyển nguyên lịch sử sang Google, trừ
khi bạn chủ động muốn gửi lịch sử đó sang Gemini.

## API và model

Khi chọn Gemini, endpoint localhost và API key hoạt động như trước:

- `POST /v1/messages`: Anthropic Messages JSON/SSE, text, ảnh base64, tool calls.
- `POST /v1/messages/count_tokens`: ước lượng token, không gọi model.
- `POST /v1/chat/completions`: OpenAI JSON/SSE, text, ảnh data URL, function tools.
- `GET /v1/models`: chỉ quảng bá `gemini` cho backend đang chọn.
- `GET /v1/provider/check`: kiểm tra đăng nhập Google, cần API key, không tạo chat.
- `GET /v1/provider/models`: catalog/quyền và mức tư duy của acc đã cấu hình;
  cần API key, chỉ lấy khi gọi, không tạo chat.

Model alias của client vẫn là **gemini**. Từ beta.10, model và mức tư duy upstream
được chọn qua **Tài khoản → Model / tư duy**, lưu riêng từng acc bằng DPAPI.
Danh sách tải từ `GetUserStatus`/mode policy của đúng số acc Google; không dựa
vào catalog hardcode. Menu mới trả model cùng Low/Medium/High và routing variant;
menu cũ có Standard/Extended. Tên model/mức tư duy dùng nhãn Google trả về.
Model bị khóa hoặc bị policy thay thế không được lưu/chạy; lựa chọn cũ biến mất
thì phải chọn lại. Mặc định không chọn gì vẫn để Google quyết định.

Backend truyền selector qua `x-goog-ext-525001261-jspb` và field model/thinking
của StreamGenerate; không thêm chỉ thị suy nghĩ vào prompt hay tự đổi model khi
lỗi. Khi chọn model cụ thể, quyền được kiểm tra khi kết nối và cache tối đa 15
phút giữa các lượt; không tải catalog mỗi request. Lưu thay đổi của acc đang chạy
dừng server. Kết nối lại và mở phiên Claude mới để áp dụng lựa chọn. Client không
tự đổi upstream bằng `/model`; alias vẫn là `gemini` cho main và subagent.

Mode policy mới dùng category + routing variant (header field 25), còn model
catalog cũ dùng hash; không đưa chuỗi `argon` vào chỗ hash như model cũ. Mức tư
duy cũng có routing variant riêng (header field 26) nếu Google cung cấp.
Đã thử Google thật với acc 5: Hydrogen Thấp trả `OK` khoảng 4 giây, Trung bình
khoảng 20 giây. Argon đã lấy được quyền/lựa chọn nhưng các lượt thử StreamGenerate
bị timeout 150 giây, chưa xác nhận generation thành công. Không tự chuyển sang
Hydrogen khi gặp timeout; đây vẫn là giới hạn thực tế cần kiểm chứng thêm.

Google multi-login dùng cùng cookie cho nhiều acc. `GeminiAccountIndex` phải
khớp URL `/u/N/app`; bootstrap, catalog, quota và generation dùng cùng N.
Không tìm thấy acc N thì báo lỗi, không quay về acc 0. Metadata phiên/cookie/cache
không chia sẻ giữa các cấu hình. Request `muse` khi backend Gemini đang chạy bị
từ chối, không fallback qua Muse; chiều ngược lại cũng vậy.

Lệnh `cy`/nút mở Claude đọc backend từ cấu hình mã hóa; main model,
Haiku/Sonnet/Opus và subagent đều được pin theo backend hiện tại. Exa MCP tiếp tục
cung cấp web search cho Claude Code.

## Kiểm tra quota

Từ beta.9, bấm **Kiểm tra quota** ở dòng Gemini trong **Tài khoản**. Không cần
chuyển sang tài khoản đó hoặc bật server. App gọi read-only UsagePageQuery của
`gemini.google.com/usage` bằng đúng cookie/proxy tài khoản đã bấm; không tạo chat,
không gọi quota Muse, không tự tải khi mở app/chọn acc/đổi tab hoặc theo timer.

Kết quả có **5 giờ** và **hàng tuần**, % đã dùng và giờ reset theo giờ máy. Google
trả tỷ lệ 0–1; app đổi sang % như web, không suy ra số prompt/token còn lại. Kết
quả giữ riêng từng acc trong RAM cùng thời điểm kiểm tra; bấm lại để cập nhật.
Thiếu một cửa sổ thì báo chưa có dữ liệu cho cửa sổ đó. Không có dữ liệu hoặc
cookie hết hạn thì báo rõ, không coi là 0% hay không giới hạn. Quota Gemini Web
khác Gemini API/CLI/Antigravity.

Request có hạn 20 giây và tối đa 256 KiB; không redirect/retry/fallback trực tiếp
khi proxy lỗi. Google có thể đổi persisted query/schema, khi đó cần cập nhật app.
Giao thức tham khảo từ [CodexBar Gemini Web](https://github.com/steipete/CodexBar/pull/4535)
và [hướng dẫn hạn mức Gemini Apps](https://support.google.com/gemini/answer/16275805?hl=en).
Đã kiểm tra HTTP giả lập, proxy CONNECT và nút UI. Ngày 13/11/2026, bấm nút trong
app với cookie Google thật đã đọc thành công cả hai cửa sổ. Lượt kiểm tra beta.9
trước khi bổ sung account index dùng acc 0, không phải acc 5 người dùng muốn chọn;
beta.10 dùng số acc đã lưu cho cả quota/model/chat. Không suy ra hạn mức của các
tài khoản khác từ kết quả này.

Đường OpenAI của Gemini gửi toàn bộ lịch sử client; hỗ trợ function `tools`,
`tool_choice` và `parallel_tool_calls`. Ảnh chỉ nhận base64/data URL; không tải URL
do client tùy ý cung cấp. `conversation_id`, Responses API, Google-native API,
audio/PDF, structured output và Anthropic extended-thinking blocks chưa hỗ trợ.
Mức tư duy native Gemini chọn trong app là điều khiển riêng, không trả thinking
blocks theo định dạng Anthropic.

## Phiên và tính đúng đắn

Anthropic dùng chung cơ chế prefix/cache hiện có: chỉ khi lịch sử, system và tool
definitions khớp mới gửi delta. Mỗi nhánh/subagent có metadata hội thoại Gemini
riêng; sửa/compact/nhánh mới mở phiên độc lập. Các metadata này chỉ giữ trong RAM.
Ảnh đã upload được tái sử dụng riêng trong hội thoại; lỗi upload không bỏ ảnh rồi
giả vờ tiếp tục thành công.

Tool calling vẫn qua prompt JSON và kiểm tra schema toàn bộ, như Muse. Phản hồi
được giữ tới khi upstream xác nhận hoàn tất và JSON/tool input hợp lệ; SSE cung
cấp giao thức streaming cho client, chưa phải phát trực tiếp token Gemini. Stream
đứt/truncated hoặc thiếu dấu hoàn tất không phát tool call hay báo thành công.
HTTP 429/network failure không tự đổi tài khoản, không tự chuyển provider và không
tự gửi lại generation có thể đã được Google nhận.

Bootstrap lấy token/build label từ trang Gemini đã đăng nhập. Không dùng cookie
hợp lệ thì báo lỗi, không lặng lẽ chuyển sang anonymous. Set-Cookie từ Gemini
được cập nhật trong RAM cho đúng tài khoản; chưa lưu cookie xoay mới xuống DPAPI
hay tự gia hạn từ browser. Khi cookie hết hạn, cập nhật thủ công trong app.
Cookie Google không gửi tới Muse hoặc host upload ảnh. Toàn bộ HTTP/upload dùng
proxy tài khoản, không fallback trực tiếp nếu proxy lỗi.

## Chạy Rust riêng

```powershell
$env:MUSE_BACKEND = 'gemini'
$env:GEMINI_COOKIE = '<Cookie Gemini bạn tự cung cấp>'
$env:GEMINI_ACCOUNT_INDEX = '5' # số trong /u/5/app; mặc định 0
$env:GEMINI_MODEL_ID = '' # selector lấy từ catalog; rỗng = Google mặc định
$env:GEMINI_THINKING_ID = '' # ID mức từ catalog, không phải token budget
$env:MUSE_PROXY_API_KEY = '<key localhost do bạn đặt>'
$env:MUSE_UPSTREAM_PROXY = '' # hoặc proxy HTTP/HTTPS riêng
cargo run
```

Launcher ngoài app: đặt `MUSE_MODEL=gemini`, `MUSE_PROXY_URL` và
`MUSE_PROXY_API_KEY`, rồi chạy `claude-muse.ps1`. Trong app không cần đặt các biến
này thủ công. Mặc định không đặt `MUSE_BACKEND` thì giữ đường Muse cũ.

## Kiểm chứng và giới hạn

- Test HTTP giả lập: cookie/bootstrap/rotation RAM, hội thoại độc lập, metadata
  nối tiếp, Unicode chia ở mọi byte, ảnh upload/cache và không rò cookie.
- Test API: auth, từ chối model sai, Anthropic JSON/SSE, OpenAI function tools,
  lịch sử nhiều tool result, ảnh và `[DONE]`.
- Claude Code cài trên máy thực sự đọc file fixture qua adapter Gemini + server
  Google giả lập, rồi tiếp tục cùng metadata và nhận kết quả đọc file.
- Kiểm tra desktop: migration/DPAPI, chuyển Muse↔Gemini, launcher model overrides,
  Gemini child không bootstrap Muse; cookie Gemini không đi vào quota Muse.
- **Chưa xác minh generation trên tài khoản Google thật**: chưa có cookie Gemini
  được người dùng cung cấp. Giao thức web có thể thay đổi hoặc yêu cầu cơ chế
  trình duyệt khác; test giả lập không thay thế việc kiểm chứng đó.

Đây là tích hợp không chính thức; quota/quyền truy cập của Gemini Web không tương
đương Gemini Developer API. Context 256k hiện là budget client, không phải giới
hạn Google đã xác minh. Cookie Google cho phép truy cập tài khoản: chỉ lưu cục bộ.

Nguồn nghiên cứu giao thức (không nhúng/chạy các dự án này):
- https://github.com/n0madic/go-gemini-web2api
- https://github.com/HanaokaYuzu/Gemini-API
- https://github.com/py7hon/gemini-web-api

Các repo chỉ là tham khảo cho định dạng HTTP/token/framing; cấu trúc backend,
quản lý session/ảnh, validation tool, lỗi và DPAPI dùng nền tảng Muse Desk hiện có.
