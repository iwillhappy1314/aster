#ifndef ASTER_MARKDOWN_H
#define ASTER_MARKDOWN_H

#ifdef __cplusplus
extern "C" {
#endif

char *aster_markdown_to_html(const char *markdown);
void aster_string_free(char *value);

#ifdef __cplusplus
}
#endif

#endif
