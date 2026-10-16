# 비공개 시험 자료

이 폴더에는 오픈소스가 아닌 시험 자료를 **암호화한 채로만** 둡니다.

| 파일 | 원본 | 원본 SHA-256 |
| --- | --- | --- |
| `openspatial-upmixer.vst3.gpg` | OpenSpatial Upmixer (VST 3, x64, 제3자 비공개 플러그인) | `9ddcd1a9a71a4f0228754f6e6eb5b08715330fa80cfc11dd0570796c806555bd` |

- 암호화: GnuPG 대칭 암호(AES-256, S2K SHA-512)입니다. 복호화 암호는 저장소 Secret `UPMIX_PLUGIN_KEY`에만 있습니다.
- 푸는 곳: Windows CI 러너의 `$RUNNER_TEMP` 안에서만 풉니다(`decrypt-upmix-plugin.sh`). 저장소 안에 풀지 않습니다.
- 금지:
  - 푼 파일을 아티팩트, 캐시, 릴리스, 갤러리 브랜치에 올리지 않습니다.
  - 푼 파일을 로그에 덤프하지 않습니다.
  - 이 폴더의 `.gitignore`가 푼 파일(`*.vst3`, `*.dll`)의 커밋을 막습니다.
- Secret이 없으면(포크에서 온 PR 등) 이 자료를 쓰는 시험은 건너뜁니다. 저장소 안에서 만든 시험용 플러그인 시험은 그대로 돕니다.
