# Đối chiếu tính năng và khoảng thiếu

Ngày kiểm tra: 12/11/2026. Đây là review source, không phải benchmark hoặc kiểm thử chạy hai proxy Python. Không thay đổi runtime trong lượt review này.

## Cập nhật sau review

Đã triển khai trong lượt tiếp theo:

- Tách message_done khỏi task completion; theo dõi root/task con và reply mở, nhiều reply, progress, trạng thái failed/cancelled từ Muse.
- Chống lặp sequence/message hoàn tất, phát hiện final text bị thay đổi sau khi đã gửi, giữ widget ở các reply tiếp theo.
- JSON `stream=false`, model catalog, text parts, chính sách lấy user message cuối, kiểm tra các capability chưa hỗ trợ.
- Socket/khóa theo conversation, tối đa 4 conversation active, 64 lượt active/chờ, cache 32 socket; bootstrap tuần tự, giới hạn queue 15 giây.
- Bỏ remote session lookup thừa trước mỗi send; history fallback chỉ bắt đầu sau 2 giây im lặng nội dung realtime.

Các giới hạn còn lại: không tự gửi cancel upstream khi client ngắt; chưa có API key/Origin policy; history-only completion vẫn dùng quiet-window heuristic; tạo conversation chưa nằm trong deadline tổng; chưa refactor toàn bộ chat theo port; chưa giải quyết shared storage/account pool/metrics. Các mục dưới đây là ghi nhận tại thời điểm review ban đầu, không phải toàn bộ đều còn nguyên.

Cập nhật Claude Code: đã thêm `/v1/messages`, `/v1/messages/count_tokens`, key
riêng cho hai route này, outbound port `TextGenerator` và prompt-based tool bridge
có kiểm tra JSON Schema. System/history/tool results được giữ đủ trong chế độ
Anthropic. Đã thử Read → Write qua Muse thật; xem [chi tiết](CLAUDE_CODE.md).
Các route OpenAI/conversation cũ vẫn chưa có auth; chưa có Origin policy chung.

CLI test đã được gỡ. Các ghi nhận CLI bên dưới là lịch sử của bản review ban đầu; sử dụng HTTP API hoặc Claude Code để kiểm tra hiện tại.

## Nguồn tham khảo

- `uezo/muse-proxy`, commit `627fbfbee3593a54862122eb50ff28c299b5836f`: [reference](https://github.com/uezo/muse-proxy/blob/627fbfbee3593a54862122eb50ff28c299b5836f/docs/reference.md), [routing](https://github.com/uezo/muse-proxy/blob/627fbfbee3593a54862122eb50ff28c299b5836f/src/muse_proxy/_routing.py), [credentials](https://github.com/uezo/muse-proxy/blob/627fbfbee3593a54862122eb50ff28c299b5836f/src/muse_proxy/credentials.py). Dùng Gadget pairing và SDK; không dùng cùng cơ chế cookie login với Rust hiện tại.
- `d4ncboz/museai`, commit `b470724`: [gateway](https://github.com/d4ncboz/museai/blob/b470724/src/museai/services/gateway.py), [pool](https://github.com/d4ncboz/museai/blob/b470724/src/museai/accounts/pool.py), [HTTP driver](https://github.com/d4ncboz/museai/blob/b470724/src/museai/drivers/http/driver.py), [Responses route](https://github.com/d4ncboz/museai/blob/b470724/src/museai/api/routes/responses.py). Browser driver là đường xử lý chat có triển khai; HTTP chat driver và Responses route vẫn trả lỗi chưa triển khai.

## Phần Rust đã có

Cookie → VM/token → Noise XX/WebSocket → protobuf; ghép chunk xen kẽ có giới hạn bộ nhớ; SSE và CLI; conversation riêng, lưu mapping và resume; preflight ping/reconnect; xử lý client disconnect trong lượt chat; kênh output có giới hạn; widget gợi ý dạng text/metadata. Không cần Chromium cho các ca đã thử thật.

## Ưu tiên 1: tính đúng của lượt chat

1. **Một message kết thúc chưa có nghĩa toàn bộ task kết thúc.** `core/turn.rs` chỉ nhận một số sự kiện message, bỏ qua `task.status` và `agent.status`. `adapters/muse/session.rs` kết thúc vòng đọc khi gặp `MessageDone`. Hệ quả có thể bỏ mất reply tiếp theo, kết quả tool hoặc widget gửi sau đó, đồng thời không biểu diễn đúng task thất bại/hủy. Cần theo dõi root task, các message đang mở và task con; chỉ xuất `[DONE]` khi lượt thực sự kết thúc. `uezo` có xử lý loại vòng đời này. Đây là thiếu sót về hành vi, không chỉ là thiếu endpoint.

2. **Tiến độ và hủy task.** Hiện mới ngắt socket khi client rời đi, chưa gửi lệnh hủy Muse. Cần phân biệt client ngừng chờ, Muse đã hủy, task lỗi và task hoàn tất. Thêm progress có cấu trúc để CLI báo đang làm gì trong thời gian chưa có text; không suy diễn progress thành nội dung trả lời.

3. **Replay và nhiều reply.** Bổ sung regression cho nhiều assistant message trong một lượt, task completed đến trước message cuối, ACK muộn, event lặp sau reconnect, final text thay đổi sau khi đã stream, và widget xuất hiện sau message đầu. Các test hiện tại đã bao phủ framing, ACK muộn, fallback lịch sử, reconnect và widget cơ bản nhưng chưa bao phủ toàn bộ vòng đời đó.

## Ưu tiên 2: tương thích API và sử dụng ổn định

4. **API hiện còn hẹp.** `api/chat.rs` chỉ hỗ trợ một message user dạng string, `stream=true`, model `muse`. Thiếu JSON response khi `stream=false`, `/v1/models`, text-part arrays và chính sách xử lý `messages` nhiều phần tử. Phải công bố rõ gửi lại history hay chỉ lấy user message cuối; không gửi lại cả history vào một session đã có ngữ cảnh mà không kiểm tra trùng.

5. **Tham số chưa hỗ trợ đang dễ bị bỏ qua âm thầm.** Struct Deserialize mặc định bỏ qua trường lạ. Ví dụ client gửi `tools`, `response_format` hay `stop` có thể nhận câu trả lời text mà không biết yêu cầu không được thực hiện. Nên phân loại trường chấp nhận-bỏ qua và trường phải trả 400; đồng nhất error envelope, error code và request ID.

6. **Xác thực đầu vào.** Hiện không có API key hay kiểm tra Origin cho các endpoint điều khiển tài khoản Muse. Trước khi dùng chung hoặc mở ra ngoài localhost cần Bearer key, quy tắc Origin rõ ràng, tách quyền admin nếu thêm quản lý account. Không coi cookie Muse là key đưa cho ứng dụng client.

7. **Deadline và hàng chờ.** `connection.lock()` được giữ trong cả lượt; deadline 120 giây bắt đầu sau khi đã chờ khóa và chuẩn bị kết nối. Cần giới hạn số request chờ, deadline tổng gồm queue/bootstrap và lỗi 429/503 khi quá tải. Kênh output hiện đã bounded, nên không cần thay thế bằng hàng đợi không giới hạn.

## Ưu tiên 3: hiệu năng và cấu trúc

8. **Khóa toàn tài khoản làm các conversation chặn nhau.** Tách actor/quản lý vòng đời theo conversation. Có thể dùng socket riêng theo conversation như `uezo`, hoặc một reader task duy nhất trên socket chung rồi phân phối frame theo stream ID. Không tạo nhiều vòng decrypt cạnh tranh trên cùng Noise state. Vẫn giữ giới hạn concurrency cấp account.

9. **Có round trip thừa.** Với conversation cũ: API gọi `conversation()` để ping và kiểm tra session; `run()` lại ping; tiếp theo đọc baseline history. Trong lúc stream, lịch sử còn được poll mỗi giây dù đang có delta. Nên tách lookup mapping khỏi xác minh remote, dùng health-check freshness và chỉ bật fallback khi realtime im lặng theo chính sách rõ ràng.

10. **Hexagonal chưa hoàn chỉnh ở luồng chat thật.** `api/chat.rs` phụ thuộc trực tiếp vào `MuseSession`. Adapter Muse tự tạo `FileConversationStore` và đọc env. Port `MessageHandler` hiện phục vụ echo. Cần use case chat/conversation và port backend thực sự; composition root trong `lib.rs` xây config, backend, store rồi inject chúng. Không chỉ tách thêm file hoặc tạo trait cho mọi struct.

11. **Storage và account lifecycle.** File JSON hiện đọc cả file và khóa trong process, phù hợp một instance. SQLite là bước tiếp theo hợp lý cho metadata lớn hơn; nhiều instance cần cơ chế phối hợp/shared store. Mapping đang gắn với VM: nếu VM thay đổi cần quy trình xác minh/migration thay vì giả định account mới. Bootstrap lại token đã có; cookie hết hạn vẫn cần cơ chế cập nhật credential có kiểm soát. Paired credential refresh của `uezo` không thể áp dụng nguyên xi cho cookie login.

12. **Vận hành.** Bổ sung readiness riêng với liveness, request ID nối HTTP ↔ stream ↔ Muse message, số request chờ, first-content latency và tỷ lệ reconnect. Cần đóng các task/connection trong shutdown, cấu hình timeout/throttling tập trung và hướng dẫn khởi động credential an toàn. Không chạy retry user message một cách mù quáng sau khi đã gửi lên Muse.

## Hạng mục tùy nhu cầu

- Account pool, affinity, cooldown và admin API: tham khảo `museai` khi có nhiều account. Conversation phải giữ đúng account; không failover sang account khác rồi tiếp tục như thể ngữ cảnh vẫn còn.
- Ảnh, file, video, widget state đầy đủ: làm khi cần ngoài text chat. API hiện trả nhãn lựa chọn, chưa đồng bộ trạng thái widget trên Muse.
- `/v1/responses`, tool calling và adapter cho các client khác: cần thiết kế/test riêng nếu đó là mục tiêu. Hai repo trên không cung cấp sẵn một Responses implementation hoàn chỉnh để chép sang Rust.
- Alias GPT/Claude không đồng nghĩa chọn được model GPT/Claude thật. Không bổ sung alias hoặc token usage ước lượng rồi trình bày như số liệu upstream đã xác nhận.

Thứ tự đề xuất: vòng đời task/nhiều reply → API contract và auth → queue/deadline → actor theo conversation → giảm ping/poll thừa → storage/account pool/media theo nhu cầu.
