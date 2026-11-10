# Thiết lập Microsoft Authentication (M6)

Tài liệu này hướng dẫn đăng ký Azure app để MangoLauncher đăng nhập Microsoft
(Xbox Live → Minecraft Services). Không có client ID đã được Mojang duyệt thì
không thể test luồng này đầu-cuối.

## TL;DR — 5 bước

1. Azure Portal → **Microsoft Entra ID** → **App registrations** → **New registration**
2. Supported account types: **Personal Microsoft accounts only**
3. Redirect URI: platform **Public client/native (mobile & desktop)**, giá trị `http://localhost`
4. **Authentication** → **Advanced settings** → **Allow public client flows** = **Yes**
5. Gửi client ID tới <https://aka.ms/mce-reviewappid> để Mojang allow-list

Bước 5 là bắt buộc. Bỏ qua nó, login sẽ fail ở bước cuối với
`403 Invalid app registration` dù OAuth và XSTS đều thành công.

---

## 1. Đăng ký app

1. Đăng nhập <https://portal.azure.com>
2. Tìm **Microsoft Entra ID** → **App registrations** → **New registration**
3. Điền:
   - **Name**: `MangoLauncher` (tên gì cũng được, hiện cho user lúc đăng nhập)
   - **Supported account types**: **Personal Microsoft accounts only**
     (đúng cho launcher; chọn nhầm "single tenant" thì account thường không login được)
   - **Redirect URI**: chọn **Public client/native (mobile & desktop)**,
     nhập chính xác `http://localhost`
4. **Register**, rồi copy **Application (client) ID** ở trang Overview

### Vì sao `http://localhost` chứ không phải `nativeclient`

Hai kiểu redirect cùng hợp lệ, khác nhau ở cách launcher bắt code:

| Redirect URI | Cách nhận code | Ghi chú |
|---|---|---|
| `https://login.microsoftonline.com/common/oauth2/nativeclient` | Nhúng webview, đọc URL khi nó điều hướng | Helios dùng cách này; cần webview |
| `http://localhost` | Mở browser hệ thống, `TcpListener` trên `127.0.0.1:<port ngẫu nhiên>` | MangoLauncher dùng cách này |

Với `http://localhost`, Microsoft **bỏ qua port** cho loopback redirect trên public
client, nên launcher bind port trống bất kỳ mà không cần đăng ký từng port. Đây
cũng là khuyến nghị hiện tại của Microsoft cho desktop app (MSAL dùng
`https://localhost` cho .NET vì không có UI nhúng). Không nhúng webview nghĩa là
user thấy đúng thanh địa chỉ của Microsoft khi nhập mật khẩu — quan trọng về mặt
tin cậy và tránh bị coi là phishing.

## 2. Cho phép public client flow

**Authentication** → kéo xuống **Advanced settings** → **Allow public client flows**
= **Yes** → **Save**.

Cần cho device-code flow. Launcher là public client, dùng PKCE (S256), **không có
client secret**. Đừng tạo secret — secret trong app desktop là secret bị lộ.

## 3. Xin Mojang allow-list

Từ 2022, app Azure mới **không** gọi được Minecraft Services API cho tới khi Mojang
duyệt. Trước khi duyệt, login fail ở bước cuối:

```text
HTTP 403 — "Invalid app registration" / "Minecraft services rejected this app"
```

Gửi form: <https://aka.ms/mce-reviewappid>

Chuẩn bị:
- Application (client) ID
- Tenant ID (Overview page)
- Mô tả ngắn về launcher, có phải non-commercial hay không
- Link repo/wiki nếu có

Lưu ý thực tế:
- Duyệt **theo client ID**, không theo build. Duyệt rồi thì mọi bản build dùng ID đó
  login được, không cần build lại.
- Thời gian duyệt không đoán trước được, có trường hợp mất nhiều tuần.
- **Phải thử login ít nhất một lần trước khi xin duyệt** — form hỏi về lỗi bạn gặp.
  Đây là yêu cầu tường minh trong tài liệu gốc của Helios.

## 4. Cấu hình client ID cho launcher

Client ID **không phải secret** — nó nằm trong mọi binary phát hành. Nhưng nên để
ngoài git để fork tự đăng ký app riêng.

Thứ tự ưu tiên (sẽ implement ở M6):

1. Biến môi trường — tốt cho dev:
   ```powershell
   $env:MANGO_MSA_CLIENT_ID = "00000000-0000-0000-0000-000000000000"
   cargo run -p mango-ui
   ```
2. File cấu hình: `%APPDATA%\MangoLauncher\msa.json`
   ```json
   { "clientId": "00000000-0000-0000-0000-000000000000" }
   ```
3. Bake lúc build từ env var (dùng trong CI với GitHub secret)

## 5. Luồng token

Sau khi có code từ browser, chuỗi gọi API là:

```text
1. POST login.microsoftonline.com/consumers/oauth2/v2.0/token
   scope = XboxLive.signin offline_access
   grant_type = authorization_code, code_verifier (PKCE)
   → { access_token, refresh_token, expires_in }

2. POST user.auth.xboxlive.com/user/authenticate
   RpsTicket = "d=<msa access_token>"
   → { Token, DisplayClaims.xui[0].uhs }

3. POST xsts.auth.xboxlive.com/xsts/authorize
   RelyingParty = "rp://api.minecraftservices.com/"
   → { Token, DisplayClaims.xui[0].uhs }

4. POST api.minecraftservices.com/authentication/login_with_xbox
   identityToken = "XBL3.0 x=<uhs>;<xsts token>"
   → { access_token, expires_in }   ← đây là accessToken đưa cho game

5. GET api.minecraftservices.com/minecraft/profile
   Bearer <mc access_token>
   → { id, name, skins, capes }
```

Lỗi XSTS thường gặp và ý nghĩa:

| XErr | Nghĩa | Thông báo cho user |
|---|---|---|
| `2148916233` | Account chưa có Xbox profile | "Đăng nhập Xbox.com để tạo profile trước" |
| `2148916235` | Account bị ban khỏi Xbox Live | "Account này bị chặn khỏi Xbox Live" |
| `2148916238` | Account trẻ em, chưa có parental consent | "Cần người giám hộ đồng ý" |

Hai lỗi cuối hiếm gặp nhưng nếu không bắt sẽ ra thông báo chung chung
"đăng nhập thất bại", rất khó debug cho user.

## 6. Lưu token

- Minecraft access token + MSA refresh token → **OS keyring**
  (DPAPI trên Windows, Secret Service trên Linux, Keychain trên macOS)
- `config.json` chỉ lưu metadata: `type`, `uuid`, `displayName`, `expiresAt`
- **Không bao giờ** ghi token vào `config.json` hay log

Refresh logic 3 nhánh:

```text
MC token còn hạn          → dùng luôn
MC hết hạn, MSA còn hạn   → chạy lại bước 2–5 với MSA access token cũ
MC hết hạn, MSA hết hạn   → refresh_token → bước 1 → 2–5
```

Trừ hao 10 giây khi tính `expiresAt` để tránh dùng token vừa hết hạn.

## 7. Checklist trước khi báo "M6 xong"

- [ ] Đăng nhập được, thấy username + skin head trong UI
- [ ] `config.json` **không** chứa token (grep để chắc chắn)
- [ ] Token nằm trong keyring (kiểm tra bằng `cmdkey /list` trên Windows)
- [ ] Restart app → vẫn còn đăng nhập
- [ ] Kill token (hoặc chờ hết hạn) → refresh thành công
- [ ] Xoá account → chọn account khác → token cũ bị invalidate
- [ ] Test account chưa mua Minecraft → báo "chưa sở hữu game", không phải lỗi chung
- [ ] Test account không có Xbox profile → đúng thông báo `2148916233`

## Tham chiếu

- Helios Launcher — `docs/MicrosoftAuth.md` (quy trình gốc, dùng `nativeclient`)
- Microsoft — [Application configuration options](https://learn.microsoft.com/en-us/entra/identity-platform/msal-client-application-configuration)
- Arctic Launcher — [Microsoft sign-in](https://docs.arcticlauncher.com/microsoft-auth) (loopback + device code, đã được duyệt)
- Mojang app review — <https://aka.ms/mce-reviewappid>
