table 50101 "FX Plain"
{
    fields
    {
        field(1; Id; Integer)
        {
        }
    }
}

page 50101 "FX Plain Page"
{
    SourceTable = "FX Plain";

    layout
    {
        area(Content)
        {
            field(Id; Rec.Id)
            {
                ApplicationArea = All;
            }
        }
    }
}

query 50101 "FX Plain Query"
{
    elements
    {
        dataitem(Plain; "FX Plain")
        {
            column(Id; Id)
            {
            }
        }
    }
}

xmlport 50101 "FX Plain Port"
{
    schema
    {
        textelement(Root)
        {
        }
    }
}

report 50101 "FX Plain Report"
{
    ProcessingOnly = true;

    dataset
    {
        dataitem(Plain; "FX Plain")
        {
            column(Id; Id)
            {
            }
        }
    }
}
